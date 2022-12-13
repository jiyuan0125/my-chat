use async_std::io;
use futures::{
    prelude::{stream::StreamExt, *},
    select,
};
use libp2p::{
    floodsub::{self, Floodsub, FloodsubEvent},
    identity, mdns,
    swarm::{NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Swarm,
};
use log::{error, info};

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "ChatOutEvent")]
struct ChatBehaviour {
    floodsub: Floodsub,
    mdns: mdns::async_io::Behaviour,
}

#[derive(Debug)]
enum ChatOutEvent {
    Floodsub(FloodsubEvent),
    Mdns(mdns::Event),
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

fn get_public_topic() -> floodsub::Topic {
    get_topic("public")
}

fn get_topic(topic_name: &str) -> floodsub::Topic {
    floodsub::Topic::new(format!("chat_{}", topic_name))
}

async fn app_loop(
    name: &str,
    listen_addr: Option<&str>,
    to_dial: Option<&str>,
) -> anyhow::Result<()> {
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer id: {local_peer_id:?}");

    let transport = libp2p::development_transport(local_key).await?;

    let mut swarm = {
        let mdns = mdns::async_io::Behaviour::new(mdns::Config::default())?;
        let behaviour = ChatBehaviour {
            floodsub: Floodsub::new(local_peer_id),
            mdns,
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
    let mut stdin = io::BufReader::new(io::stdin()).lines().fuse();
    loop {
        select! {
            line = stdin.select_next_some() => match line {
                Ok(line) => {
                    if line.starts_with('@') {
                        // Send a private message.
                        let mut parts = line.splitn(2, ' ');
                        if let (Some(name), Some(msg)) = (parts.next(), parts.next()) {
                            let topic = get_topic(name.trim_start_matches('@'));
                            swarm.behaviour_mut().floodsub.publish_any(topic.clone(), msg.as_bytes());
                            info!("Sent private message to {topic:?}: {msg}");
                        }
                    } else {
                        // Send a public message.
                        swarm.behaviour_mut().floodsub.publish(public_topic.clone(), line.as_bytes());
                        info!("Sent public message: {line}");
                    }
                }
                Err(e) => {
                    error!("Error reading from stdin: {e}");
                }
            },
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!("Listening on {address:?}");
                }
                SwarmEvent::Behaviour(ChatOutEvent::Floodsub(
                    FloodsubEvent::Message(message)
                )) => {
                    eprintln!(
                        "Received: '{:?}' from {:?}",
                        String::from_utf8_lossy(&message.data),
                        message.source
                    );
                }
                SwarmEvent::Behaviour(ChatOutEvent::Mdns(
                    mdns::Event::Discovered(list)
                )) => {
                    for (peer, _) in list {
                        swarm
                            .behaviour_mut()
                            .floodsub
                            .add_node_to_partial_view(peer);
                    }
                }
                SwarmEvent::Behaviour(ChatOutEvent::Mdns(mdns::Event::Expired(
                    list
                ))) => {
                    for (peer, _) in list {
                        if !swarm.behaviour_mut().mdns.has_node(&peer) {
                            swarm
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

pub async fn run(
    name: &str,
    listen_addr: Option<&str>,
    to_dial: Option<&str>,
) -> anyhow::Result<()> {
    app_loop(name, listen_addr, to_dial).await?;
    Ok(())
}
