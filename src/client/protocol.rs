use super::pb::chat_pb;
use super::topic::Topic;
use futures::{
    io::{AsyncRead, AsyncWrite},
    AsyncWriteExt, Future,
};
use libp2p_core::{upgrade, InboundUpgrade, OutboundUpgrade, PeerId, UpgradeInfo};
use prost::Message;
use std::{io, iter, pin::Pin};

/// Implementation of `ConnectionUpgrade` for the chat protocol.
#[derive(Debug, Clone, Default)]
pub struct ChatProtocol {}

static CHAT_PROTOCOL_NAME: &[u8] = b"/chat/1.0.0";

impl UpgradeInfo for ChatProtocol {
    type Info = &'static [u8];
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(CHAT_PROTOCOL_NAME)
    }
}

impl<TSocket> InboundUpgrade<TSocket> for ChatProtocol
    where
        TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = ChatRpc;
    type Error = ChatDecodeError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, mut socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            let packet = upgrade::read_length_prefixed(&mut socket, 2048).await?;
            let rpc = chat_pb::Rpc::decode(&packet[..]).map_err(DecodeError)?;

            let mut messages = Vec::with_capacity(rpc.publish.len());
            for publish in rpc.publish.into_iter() {
                messages.push(ChatMessage {
                    source: PeerId::from_bytes(&publish.from_peer_id.unwrap_or_default())
                        .map_err(|_| ChatDecodeError::InvalidPeerId)?,
                    source_name: publish.from_name.unwrap_or_default(),
                    to_name: publish.to_name,
                    data: publish.data.unwrap_or_default(),
                    sequence_number: publish.seqno.unwrap_or_default(),
                    topics: publish.topic_ids.into_iter().map(Topic::new).collect(),
                });
            }

            Ok(ChatRpc {
                messages,
                subscriptions: rpc
                    .subscriptions
                    .into_iter()
                    .map(|sub| ChatSubscription {
                        action: if Some(true) == sub.subscribe {
                            ChatSubscriptionAction::Subscribe
                        } else {
                            ChatSubscriptionAction::Unsubscribe
                        },
                        topic: Topic::new(sub.topic_id.unwrap_or_default()),
                    })
                    .collect(),
            })
        })
    }
}

/// Reach attempt interrupt errors.
#[derive(thiserror::Error, Debug)]
pub enum ChatDecodeError {
    /// Error when reading the packet from the socket.
    #[error("Failed to read from socket")]
    ReadError(#[from] io::Error),
    /// Error when decoding the raw buffer into a protobuf.
    #[error("Failed to decode protobuf")]
    ProtobufError(#[from] DecodeError),
    /// Error when parsing the `PeerId` in the message.
    #[error("Failed to decode PeerId from message")]
    InvalidPeerId,
}

#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct DecodeError(prost::DecodeError);

/// An RPC received by the chat system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatRpc {
    /// List of messages that were part of this RPC query.
    pub messages: Vec<ChatMessage>,
    /// List of subscriptions.
    pub subscriptions: Vec<ChatSubscription>,
}

impl UpgradeInfo for ChatRpc {
    type Info = &'static [u8];
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(CHAT_PROTOCOL_NAME)
    }
}

impl<TSocket> OutboundUpgrade<TSocket> for ChatRpc
    where
        TSocket: AsyncWrite + AsyncRead + Send + Unpin + 'static,
{
    type Output = ();
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, mut socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            let bytes = self.into_bytes();

            upgrade::write_length_prefixed(&mut socket, bytes).await?;
            socket.close().await?;

            Ok(())
        })
    }
}

impl ChatRpc {
    /// Turns this `ChatRpc` into a message that can be sent to a substream.
    fn into_bytes(self) -> Vec<u8> {
        let rpc = chat_pb::Rpc {
            publish: self
                .messages
                .into_iter()
                .map(|msg| chat_pb::Message {
                    from_peer_id: Some(msg.source.to_bytes()),
                    from_name: Some(msg.source_name),
                    to_name: msg.to_name,
                    data: Some(msg.data),
                    seqno: Some(msg.sequence_number),
                    topic_ids: msg.topics.into_iter().map(|topic| topic.into()).collect(),
                })
                .collect(),

            subscriptions: self
                .subscriptions
                .into_iter()
                .map(|topic| chat_pb::rpc::SubOpts {
                    subscribe: Some(topic.action == ChatSubscriptionAction::Subscribe),
                    topic_id: Some(topic.topic.into()),
                })
                .collect(),
        };

        let mut buf = Vec::with_capacity(rpc.encoded_len());
        rpc.encode(&mut buf)
            .expect("Vec<u8> provides capacity as needed");
        buf
    }
}

/// A message received by the chat system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatMessage {
    /// Id of the peer that published this message.
    pub source: PeerId,

    /// Name of the source that published this message.
    pub source_name: String,

    /// Name of the destination that will receive this message.
    pub to_name: Option<String>,

    /// Content of the message. Its meaning is out of scope of this library.
    pub data: Vec<u8>,

    /// An incrementing sequence number.
    pub sequence_number: Vec<u8>,

    /// List of topics this message belongs to.
    ///
    /// Each message can belong to multiple topics at once.
    pub topics: Vec<Topic>,
}

/// A subscription received by the chat system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatSubscription {
    /// Action to perform.
    pub action: ChatSubscriptionAction,
    /// The topic from which to subscribe or unsubscribe.
    pub topic: Topic,
}

/// Action that a subscription wants to perform.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChatSubscriptionAction {
    /// The remote wants to subscribe to the given topic.
    Subscribe,
    /// The remote wants to unsubscribe from the given topic.
    Unsubscribe,
}
