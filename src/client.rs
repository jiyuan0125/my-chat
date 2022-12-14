use crate::client::layer::{Chat, ChatEvent};
use crate::client::topic::Topic;
use async_std::io;
use futures::{
    prelude::{stream::StreamExt, *},
    select,
};
use libp2p::{
    identity, mdns,
    swarm::{keep_alive::Behaviour as KeepAliveBehaviour, NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Swarm,
};
use log::{error, info};
use once_cell::sync::OnceCell;

mod layer;
mod pb;
mod protocol;
mod topic;

static TOPIC: OnceCell<Topic> = OnceCell::new();

#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// Peer id of the local node. Used for the source of the messages that we publish.
    pub local_peer_id: PeerId,

    /// `true` if messages published by local node should be propagated as messages received from
    /// the network, `false` by default.
    pub subscribe_local_messages: bool,
}

impl ChatConfig {
    pub fn new(local_peer_id: PeerId) -> Self {
        Self {
            local_peer_id,
            subscribe_local_messages: false,
        }
    }
}

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "ChatOutEvent")]
struct ChatBehaviour {
    keep_alive: KeepAliveBehaviour,
    chat: Chat,
    mdns: mdns::async_io::Behaviour,
}

#[derive(Debug)]
enum ChatOutEvent {
    KeepAlive,
    Chat(ChatEvent),
    Mdns(mdns::Event),
}

impl From<mdns::Event> for ChatOutEvent {
    fn from(v: mdns::Event) -> Self {
        ChatOutEvent::Mdns(v)
    }
}

impl From<ChatEvent> for ChatOutEvent {
    fn from(v: ChatEvent) -> Self {
        ChatOutEvent::Chat(v)
    }
}

impl From<void::Void> for ChatOutEvent {
    fn from(_v: void::Void) -> Self {
        ChatOutEvent::KeepAlive
    }
}

pub struct Client {
    name: String,
    swarm: Swarm<ChatBehaviour>,
}

impl Client {
    pub async fn new(
        name: &str,
        listen_addr: Option<&str>,
        to_dial: Option<&str>,
    ) -> anyhow::Result<Self> {
        TOPIC.get_or_init(|| "chat".parse().unwrap());

        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Local peer id: {local_peer_id:?}");

        let transport = libp2p::development_transport(local_key).await?;

        let mut swarm = {
            let mdns = mdns::async_io::Behaviour::new(mdns::Config::default())?;
            let behaviour = ChatBehaviour {
                keep_alive: KeepAliveBehaviour,
                chat: Chat::new(local_peer_id),
                mdns,
            };
            Swarm::with_threadpool_executor(transport, behaviour, local_peer_id)
        };

        swarm
            .behaviour_mut()
            .chat
            .subscribe(TOPIC.get().unwrap().clone());

        if let Some(to_dial) = to_dial {
            let addr: Multiaddr = to_dial.parse()?;
            swarm.dial(addr)?;
            info!("Dialed {to_dial}");
        }

        let listen_addr = match listen_addr {
            None => "/ip4/0.0.0.0/tcp/0",
            Some(v) => v,
        };

        swarm.listen_on(listen_addr.parse()?)?;

        Ok(Client { name: name.to_string(), swarm })
    }

    pub async fn start(&mut self) {
        let mut stdin = io::BufReader::new(io::stdin()).lines().fuse();
        loop {
            select! {
                line = stdin.select_next_some() => match line {
                    Ok(line) => {
                        let (to_name, msg) = if line.starts_with('@') {
                            let mut parts = line.splitn(2, ' ');
                            let to_name = parts.next().unwrap().trim_start_matches('@');
                            let msg = parts.next().unwrap_or("");
                            (Some(to_name), msg)
                        } else {
                            (None, line.as_str())
                        };
                        self.swarm.behaviour_mut().chat.publish_any(&self.name, to_name, TOPIC.get().unwrap().clone(), msg.as_bytes());
                    }
                    Err(e) => {
                        error!("Error reading from stdin: {e}");
                    }
                },
                event = self.swarm.select_next_some() => match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Listening on {address:?}");
                    }
                    SwarmEvent::Behaviour(ChatOutEvent::Chat(
                        ChatEvent::Message(message)
                    )) => {
                        eprintln!(
                            "{} -> {}: {}",
                            message.source_name,
                            message.to_name.unwrap_or("all".to_string()),
                            String::from_utf8_lossy(&message.data),
                        );
                    }
                    SwarmEvent::Behaviour(ChatOutEvent::Mdns(
                        mdns::Event::Discovered(list)
                    )) => {
                        for (peer, _) in list {
                            self.swarm
                                .behaviour_mut()
                                .chat
                                .add_node_to_partial_view(peer);
                        }
                    }
                    SwarmEvent::Behaviour(ChatOutEvent::Mdns(mdns::Event::Expired(
                        list
                    ))) => {
                        for (peer, _) in list {
                            if !self.swarm.behaviour_mut().mdns.has_node(&peer) {
                                self.swarm
                                    .behaviour_mut()
                                    .chat
                                    .remove_node_from_partial_view(&peer);
                            }
                        }
                    },
                    _ => {}
                }
            }
        }
    }
}
