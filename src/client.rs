use async_std::io;
use futures::{
    prelude::{stream::StreamExt, *},
    select,
};
use libp2p::{
    floodsub::{self, Floodsub, FloodsubEvent},
    identity, mdns,
    swarm::{NetworkBehaviour, SwarmEvent, keep_alive::Behaviour as KeepAliveBehaviour},
    Multiaddr, PeerId, Swarm,
};
use log::{error, info};

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "ChatOutEvent")]
struct ChatBehaviour {
    floodsub: Floodsub,
    mdns: mdns::async_io::Behaviour,
    keep_alive: KeepAliveBehaviour,
}

#[derive(Debug)]
enum ChatOutEvent {
    Floodsub(FloodsubEvent),
    Mdns(mdns::Event),
    KeepAlive,
}

impl From<mdns::Event> for ChatOutEvent {
    fn from(v: mdns::Event) -> Self {
        ChatOutEvent::Mdns(v)
    }
}

impl From<FloodsubEvent> for ChatOutEvent {
    fn from(v: FloodsubEvent) -> Self {
        ChatOutEvent::Floodsub(v)
    }
}

impl From<void::Void> for ChatOutEvent {
    fn from(_v: void::Void) -> Self {
        ChatOutEvent::KeepAlive
    }
}

fn get_public_topic() -> floodsub::Topic {
    get_topic("public")
}

fn get_topic(topic_name: &str) -> floodsub::Topic {
    floodsub::Topic::new(format!("chat_{}", topic_name))
}

pub struct Client {
    swarm: Swarm<ChatBehaviour>,
}

impl Client {
    pub async fn new(
        name: &str,
        listen_addr: Option<&str>,
        to_dial: Option<&str>,
    ) -> anyhow::Result<Self> {
        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Local peer id: {local_peer_id:?}");

        let transport = libp2p::development_transport(local_key).await?;

        let mut swarm = {
            let mdns = mdns::async_io::Behaviour::new(mdns::Config::default())?;
            let behaviour = ChatBehaviour {
                floodsub: Floodsub::new(local_peer_id),
                mdns,
                keep_alive: KeepAliveBehaviour,
            };
            Swarm::with_threadpool_executor(transport, behaviour, local_peer_id)
        };

        let public_topic = get_public_topic();
        let private_topic = get_topic(name);

        swarm
            .behaviour_mut()
            .floodsub
            .subscribe(public_topic.clone());
        info!("Subscribed to topic: {public_topic:?}");
        swarm
            .behaviour_mut()
            .floodsub
            .subscribe(private_topic.clone());
        info!("Subscribed to topic: {private_topic:?}");

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

        Ok(Client { swarm })
    }

    pub async fn start(&mut self) {
        let mut stdin = io::BufReader::new(io::stdin()).lines().fuse();
        let public_topic = get_public_topic();
        loop {
            select! {
                line = stdin.select_next_some() => match line {
                    Ok(line) => {
                        if line.starts_with('@') {
                            // Send a private message.
                            let mut parts = line.splitn(2, ' ');
                            if let (Some(name), Some(msg)) = (parts.next(), parts.next()) {
                                let topic = get_topic(name.trim_start_matches('@'));
                                self.swarm.behaviour_mut().floodsub.publish_any(topic.clone(), msg.as_bytes());
                            }
                        } else {
                            // Send a public message.
                            self.swarm.behaviour_mut().floodsub.publish(public_topic.clone(), line.as_bytes());
                        }
                    }
                    Err(e) => {
                        error!("Error reading from stdin: {e}");
                    }
                },
                event = self.swarm.select_next_some() => match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Listening on {address:?}");
                    }
                    SwarmEvent::Behaviour(ChatOutEvent::Floodsub(
                        FloodsubEvent::Message(message)
                    )) => {
                        eprintln!(
                            "{:?}>{:?}",
                            message.source,
                            String::from_utf8_lossy(&message.data),
                        );
                    }
                    SwarmEvent::Behaviour(ChatOutEvent::Mdns(
                        mdns::Event::Discovered(list)
                    )) => {
                        for (peer, _) in list {
                            self.swarm
                                .behaviour_mut()
                                .floodsub
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
                                    .floodsub
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
