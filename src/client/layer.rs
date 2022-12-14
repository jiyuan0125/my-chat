#![allow(dead_code)]
use super::protocol::{
    ChatMessage, ChatProtocol, ChatRpc, ChatSubscription,
    ChatSubscriptionAction,
};
use super::topic::Topic;
use super::ChatConfig;
use cuckoofilter::{CuckooError, CuckooFilter};
use fnv::FnvHashSet;
use libp2p_core::{connection::ConnectionId, PeerId};
use libp2p_swarm::behaviour::{ConnectionClosed, ConnectionEstablished, FromSwarm};
use libp2p_swarm::{
    dial_opts::DialOpts, NetworkBehaviour, NetworkBehaviourAction, NotifyHandler, OneShotHandler,
    PollParameters,
};
use libp2p_swarm::{ConnectionHandler, IntoConnectionHandler};
use log::warn;
use smallvec::SmallVec;
use std::collections::hash_map::{DefaultHasher, HashMap};
use std::task::{Context, Poll};
use std::{collections::VecDeque, iter};

/// Network behaviour that handles the chat protocol.
pub struct Chat {
    /// Events that need to be yielded to the outside when polling.
    events: VecDeque<
        NetworkBehaviourAction<
            ChatEvent,
            OneShotHandler<ChatProtocol, ChatRpc, InnerMessage>,
        >,
    >,

    config: ChatConfig,

    /// List of peers to send messages to.
    target_peers: FnvHashSet<PeerId>,

    /// List of peers the network is connected to, and the topics that they're subscribed to.
    // TODO: filter out peers that don't support floodsub, so that we avoid hammering them with
    //       opened substreams
    connected_peers: HashMap<PeerId, SmallVec<[Topic; 8]>>,

    // List of topics we're subscribed to. Necessary to filter out messages that we receive
    // erroneously.
    subscribed_topics: SmallVec<[Topic; 16]>,

    // We keep track of the messages we received (in the format `hash(source ID, seq_no)`) so that
    // we don't dispatch the same message twice if we receive it twice on the network.
    received: CuckooFilter<DefaultHasher>,
}

impl Chat {
    /// Creates a `Chat` with default configuration.
    pub fn new(local_peer_id: PeerId) -> Self {
        Self::from_config(ChatConfig::new(local_peer_id))
    }

    /// Creates a `Chat` with the given configuration.
    pub fn from_config(config: ChatConfig) -> Self {
        Chat {
            events: VecDeque::new(),
            config,
            target_peers: FnvHashSet::default(),
            connected_peers: HashMap::new(),
            subscribed_topics: SmallVec::new(),
            received: CuckooFilter::new(),
        }
    }

    /// Add a node to the list of nodes to propagate messages to.
    #[inline]
    pub fn add_node_to_partial_view(&mut self, peer_id: PeerId) {
        // Send our topics to this node if we're already connected to it.
        if self.connected_peers.contains_key(&peer_id) {
            for topic in self.subscribed_topics.iter().cloned() {
                self.events
                    .push_back(NetworkBehaviourAction::NotifyHandler {
                        peer_id,
                        handler: NotifyHandler::Any,
                        event: ChatRpc {
                            messages: Vec::new(),
                            subscriptions: vec![ChatSubscription {
                                topic,
                                action: ChatSubscriptionAction::Subscribe,
                            }],
                        },
                    });
            }
        }

        if self.target_peers.insert(peer_id) {
            let handler = self.new_handler();
            self.events.push_back(NetworkBehaviourAction::Dial {
                opts: DialOpts::peer_id(peer_id).build(),
                handler,
            });
        }
    }

    /// Remove a node from the list of nodes to propagate messages to.
    #[inline]
    pub fn remove_node_from_partial_view(&mut self, peer_id: &PeerId) {
        self.target_peers.remove(peer_id);
    }

    /// Subscribes to a topic.
    ///
    /// Returns true if the subscription worked. Returns false if we were already subscribed.
    pub fn subscribe(&mut self, topic: Topic) -> bool {
        if self.subscribed_topics.iter().any(|t| t.id() == topic.id()) {
            return false;
        }

        for peer in self.connected_peers.keys() {
            self.events
                .push_back(NetworkBehaviourAction::NotifyHandler {
                    peer_id: *peer,
                    handler: NotifyHandler::Any,
                    event: ChatRpc {
                        messages: Vec::new(),
                        subscriptions: vec![ChatSubscription {
                            topic: topic.clone(),
                            action: ChatSubscriptionAction::Subscribe,
                        }],
                    },
                });
        }

        self.subscribed_topics.push(topic);
        true
    }

    /// Unsubscribes from a topic.
    ///
    /// Note that this only requires the topic name.
    ///
    /// Returns true if we were subscribed to this topic.
    pub fn unsubscribe(&mut self, topic: Topic) -> bool {
        let pos = match self.subscribed_topics.iter().position(|t| *t == topic) {
            Some(pos) => pos,
            None => return false,
        };

        self.subscribed_topics.remove(pos);

        for peer in self.connected_peers.keys() {
            self.events
                .push_back(NetworkBehaviourAction::NotifyHandler {
                    peer_id: *peer,
                    handler: NotifyHandler::Any,
                    event: ChatRpc {
                        messages: Vec::new(),
                        subscriptions: vec![ChatSubscription {
                            topic: topic.clone(),
                            action: ChatSubscriptionAction::Unsubscribe,
                        }],
                    },
                });
        }

        true
    }

    /// Publishes a message to the network, if we're subscribed to the topic only.
    pub fn publish(&mut self, from_name: &str, to_name: Option<&str>, topic: impl Into<Topic>, data: impl Into<Vec<u8>>) {
        self.publish_many(from_name, to_name, iter::once(topic), data)
    }

    /// Publishes a message to the network, even if we're not subscribed to the topic.
    pub fn publish_any(&mut self, from_name: &str, to_name: Option<&str>, topic: impl Into<Topic>, data: impl Into<Vec<u8>>) {
        self.publish_many_any(from_name, to_name, iter::once(topic), data)
    }

    /// Publishes a message with multiple topics to the network.
    ///
    ///
    /// > **Note**: Doesn't do anything if we're not subscribed to any of the topics.
    pub fn publish_many(
        &mut self,
        from_name: &str,
        to_name: Option<&str>,
        topic: impl IntoIterator<Item = impl Into<Topic>>,
        data: impl Into<Vec<u8>>,
    ) {
        self.publish_many_inner(from_name, to_name, topic, data, true)
    }

    /// Publishes a message with multiple topics to the network, even if we're not subscribed to any of the topics.
    pub fn publish_many_any(
        &mut self,
        from_name: &str,
        to_name: Option<&str>,
        topic: impl IntoIterator<Item = impl Into<Topic>>,
        data: impl Into<Vec<u8>>,
    ) {
        self.publish_many_inner(from_name, to_name, topic, data, false)
    }

    fn publish_many_inner(
        &mut self,
        from_name: &str,
        to_name: Option<&str>,
        topic: impl IntoIterator<Item = impl Into<Topic>>,
        data: impl Into<Vec<u8>>,
        check_self_subscriptions: bool,
    ) {
        let message = ChatMessage {
            source: self.config.local_peer_id,
            source_name: from_name.to_string(),
            to_name: to_name.map(|s| s.to_string()),
            data: data.into(),
            // If the sequence numbers are predictable, then an attacker could flood the network
            // with packets with the predetermined sequence numbers and absorb our legitimate
            // messages. We therefore use a random number.
            sequence_number: rand::random::<[u8; 20]>().to_vec(),
            topics: topic.into_iter().map(Into::into).collect(),
        };

        let self_subscribed = self
            .subscribed_topics
            .iter()
            .any(|t| message.topics.iter().any(|u| t == u));
        if self_subscribed {
            if let Err(e @ CuckooError::NotEnoughSpace) = self.received.add(&message) {
                warn!(
                    "Message was added to 'received' Cuckoofilter but some \
                     other message was removed as a consequence: {}",
                    e,
                );
            }
            if self.config.subscribe_local_messages {
                self.events.push_back(NetworkBehaviourAction::GenerateEvent(
                    ChatEvent::Message(message.clone()),
                ));
            }
        }
        // Don't publish the message if we have to check subscriptions
        // and we're not subscribed ourselves to any of the topics.
        if check_self_subscriptions && !self_subscribed {
            return;
        }

        // Send to peers we know are subscribed to the topic.
        for (peer_id, sub_topic) in self.connected_peers.iter() {
            // Peer must be in a communication list.
            if !self.target_peers.contains(peer_id) {
                continue;
            }

            // Peer must be subscribed for the topic.
            if !sub_topic
                .iter()
                .any(|t| message.topics.iter().any(|u| t == u))
            {
                continue;
            }

            self.events
                .push_back(NetworkBehaviourAction::NotifyHandler {
                    peer_id: *peer_id,
                    handler: NotifyHandler::Any,
                    event: ChatRpc {
                        subscriptions: Vec::new(),
                        messages: vec![message.clone()],
                    },
                });
        }
    }

    fn on_connection_established(
        &mut self,
        ConnectionEstablished {
            peer_id,
            other_established,
            ..
        }: ConnectionEstablished,
    ) {
        if other_established > 0 {
            // We only care about the first time a peer connects.
            return;
        }

        // We need to send our subscriptions to the newly-connected node.
        if self.target_peers.contains(&peer_id) {
            for topic in self.subscribed_topics.iter().cloned() {
                self.events
                    .push_back(NetworkBehaviourAction::NotifyHandler {
                        peer_id,
                        handler: NotifyHandler::Any,
                        event: ChatRpc {
                            messages: Vec::new(),
                            subscriptions: vec![ChatSubscription {
                                topic,
                                action: ChatSubscriptionAction::Subscribe,
                            }],
                        },
                    });
            }
        }

        self.connected_peers.insert(peer_id, SmallVec::new());
    }

    fn on_connection_closed(
        &mut self,
        ConnectionClosed {
            peer_id,
            remaining_established,
            ..
        }: ConnectionClosed<<Self as NetworkBehaviour>::ConnectionHandler>,
    ) {
        if remaining_established > 0 {
            // we only care about peer disconnections
            return;
        }

        let was_in = self.connected_peers.remove(&peer_id);
        debug_assert!(was_in.is_some());

        // We can be disconnected by the remote in case of inactivity for example, so we always
        // try to reconnect.
        if self.target_peers.contains(&peer_id) {
            let handler = self.new_handler();
            self.events.push_back(NetworkBehaviourAction::Dial {
                opts: DialOpts::peer_id(peer_id).build(),
                handler,
            });
        }
    }
}

impl NetworkBehaviour for Chat {
    type ConnectionHandler = OneShotHandler<ChatProtocol, ChatRpc, InnerMessage>;
    type OutEvent = ChatEvent;

    fn new_handler(&mut self) -> Self::ConnectionHandler {
        Default::default()
    }

    fn on_connection_handler_event(
        &mut self,
        propagation_source: PeerId,
        _connection_id: ConnectionId,
        event: <<Self::ConnectionHandler as IntoConnectionHandler>::Handler as
        ConnectionHandler>::OutEvent,
    ) {
        // We ignore successful sends or timeouts.
        let event = match event {
            InnerMessage::Rx(event) => event,
            InnerMessage::Sent => return,
        };

        // Update connected peers topics
        for subscription in event.subscriptions {
            let remote_peer_topics = self.connected_peers
                .get_mut(&propagation_source)
                .expect("connected_peers is kept in sync with the peers we are connected to; we are guaranteed to only receive events from connected peers; QED");
            match subscription.action {
                ChatSubscriptionAction::Subscribe => {
                    if !remote_peer_topics.contains(&subscription.topic) {
                        remote_peer_topics.push(subscription.topic.clone());
                    }
                    self.events.push_back(NetworkBehaviourAction::GenerateEvent(
                        ChatEvent::Subscribed {
                            peer_id: propagation_source,
                            topic: subscription.topic,
                        },
                    ));
                }
                ChatSubscriptionAction::Unsubscribe => {
                    if let Some(pos) = remote_peer_topics
                        .iter()
                        .position(|t| t == &subscription.topic)
                    {
                        remote_peer_topics.remove(pos);
                    }
                    self.events.push_back(NetworkBehaviourAction::GenerateEvent(
                        ChatEvent::Unsubscribed {
                            peer_id: propagation_source,
                            topic: subscription.topic,
                        },
                    ));
                }
            }
        }

        // List of messages we're going to propagate on the network.
        let mut rpcs_to_dispatch: Vec<(PeerId, ChatRpc)> = Vec::new();

        for message in event.messages {
            // Use `self.received` to skip the messages that we have already received in the past.
            // Note that this can result in false positives.
            match self.received.test_and_add(&message) {
                Ok(true) => {}         // Message  was added.
                Ok(false) => continue, // Message already existed.
                Err(e @ CuckooError::NotEnoughSpace) => {
                    // Message added, but some other removed.
                    warn!(
                        "Message was added to 'received' Cuckoofilter but some \
                         other message was removed as a consequence: {}",
                        e,
                    );
                }
            }

            // Add the message to be dispatched to the user.
            if self
                .subscribed_topics
                .iter()
                .any(|t| message.topics.iter().any(|u| t == u))
            {
                let event = ChatEvent::Message(message.clone());
                self.events
                    .push_back(NetworkBehaviourAction::GenerateEvent(event));
            }

            // Propagate the message to everyone else who is subscribed to any of the topics.
            for (peer_id, subscr_topics) in self.connected_peers.iter() {
                if peer_id == &propagation_source {
                    continue;
                }

                // Peer must be in a communication list.
                if !self.target_peers.contains(peer_id) {
                    continue;
                }

                // Peer must be subscribed for the topic.
                if !subscr_topics
                    .iter()
                    .any(|t| message.topics.iter().any(|u| t == u))
                {
                    continue;
                }

                if let Some(pos) = rpcs_to_dispatch.iter().position(|(p, _)| p == peer_id) {
                    rpcs_to_dispatch[pos].1.messages.push(message.clone());
                } else {
                    rpcs_to_dispatch.push((
                        *peer_id,
                        ChatRpc {
                            subscriptions: Vec::new(),
                            messages: vec![message.clone()],
                        },
                    ));
                }
            }
        }

        for (peer_id, rpc) in rpcs_to_dispatch {
            self.events
                .push_back(NetworkBehaviourAction::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::Any,
                    event: rpc,
                });
        }
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ConnectionHandler>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        match event {
            FromSwarm::ConnectionEstablished(connection_established) => {
                self.on_connection_established(connection_established)
            }
            FromSwarm::ConnectionClosed(connection_closed) => {
                self.on_connection_closed(connection_closed)
            }
            FromSwarm::AddressChange(_)
            | FromSwarm::DialFailure(_)
            | FromSwarm::ListenFailure(_)
            | FromSwarm::NewListener(_)
            | FromSwarm::NewListenAddr(_)
            | FromSwarm::ExpiredListenAddr(_)
            | FromSwarm::ListenerError(_)
            | FromSwarm::ListenerClosed(_)
            | FromSwarm::NewExternalAddr(_)
            | FromSwarm::ExpiredExternalAddr(_) => {}
        }
    }
}

/// Transmission between the `OneShotHandler` and the `ChatHandler`.
#[derive(Debug)]
pub enum InnerMessage {
    /// We received an RPC from a remote.
    Rx(ChatRpc),
    /// We successfully sent an RPC request.
    Sent,
}

impl From<ChatRpc> for InnerMessage {
    #[inline]
    fn from(rpc: ChatRpc) -> InnerMessage {
        InnerMessage::Rx(rpc)
    }
}

impl From<()> for InnerMessage {
    #[inline]
    fn from(_: ()) -> InnerMessage {
        InnerMessage::Sent
    }
}

/// Event that can happen on the chat behaviour.
#[derive(Debug)]
pub enum ChatEvent {
    /// A message has been received.
    Message(ChatMessage),

    /// A remote subscribed to a topic.
    Subscribed {
        /// Remote that has subscribed.
        peer_id: PeerId,
        /// The topic it has subscribed to.
        topic: Topic,
    },

    /// A remote unsubscribed from a topic.
    Unsubscribed {
        /// Remote that has unsubscribed.
        peer_id: PeerId,
        /// The topic it has subscribed from.
        topic: Topic,
    },
}
