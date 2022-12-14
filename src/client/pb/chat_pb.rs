#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Rpc {
    #[prost(message, repeated, tag = "1")]
    pub subscriptions: ::prost::alloc::vec::Vec<rpc::SubOpts>,
    #[prost(message, repeated, tag = "2")]
    pub publish: ::prost::alloc::vec::Vec<Message>,
}
/// Nested message and enum types in `RPC`.
pub mod rpc {
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct SubOpts {
        #[prost(bool, optional, tag = "1")]
        pub subscribe: ::core::option::Option<bool>,
        #[prost(string, optional, tag = "2")]
        pub topic_id: ::core::option::Option<::prost::alloc::string::String>,
    }
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Message {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub from_peer_id: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    #[prost(string, optional, tag = "2")]
    pub from_name: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "3")]
    pub to_name: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub data: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub seqno: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    #[prost(string, repeated, tag = "6")]
    pub topic_ids: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}
