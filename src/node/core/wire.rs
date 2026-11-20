//! Compatibility path for the node negotiation module.

pub use veytrane_node::wire::{
    CAP_NODE_BASE, CAP_RESOURCE_AUTHORIZATION, CAP_SERVICE_DIRECTORY, MAGIC, MAX_FRAME_BYTES,
    MAX_SERVICES, NEGOTIATION_TIMEOUT, NodeErrorCode, NodeFrame, NodeMessage, REQUIRED,
    ServiceDescriptor, StreamKind, VERSION, capabilities, negotiate, read, validate_service_id,
    write,
};
