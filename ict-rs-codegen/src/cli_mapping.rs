//! Proto field type → CLI argument mapping.
//!
//! Defines how protobuf field types are rendered as Rust function parameters
//! and how they map to CLI arguments.

use prost_types::field_descriptor_proto::Type as ProtoType;

/// How a proto field is represented in generated Rust code.
#[derive(Debug, Clone)]
pub struct CliParam {
    /// Rust parameter type as a string (e.g. `&str`, `u64`, `bool`).
    pub rust_type: String,
    /// Whether this is rendered as a positional arg or `--flag value`.
    pub rendering: CliRendering,
}

/// How the CLI argument is rendered.
#[derive(Debug, Clone)]
pub enum CliRendering {
    /// `--field-name value`
    Flag,
    /// Just `value` (positional)
    Positional,
    /// `--field-name` (boolean flag, no value)
    BoolFlag,
}

/// Map a protobuf field type to its CLI representation.
pub fn map_proto_type(proto_type: ProtoType, type_name: &str) -> CliParam {
    match proto_type {
        ProtoType::String => CliParam {
            rust_type: "&str".to_string(),
            rendering: CliRendering::Flag,
        },
        ProtoType::Uint64 | ProtoType::Fixed64 => CliParam {
            rust_type: "u64".to_string(),
            rendering: CliRendering::Flag,
        },
        ProtoType::Int64 | ProtoType::Sint64 | ProtoType::Sfixed64 => CliParam {
            rust_type: "i64".to_string(),
            rendering: CliRendering::Flag,
        },
        ProtoType::Uint32 | ProtoType::Fixed32 => CliParam {
            rust_type: "u32".to_string(),
            rendering: CliRendering::Flag,
        },
        ProtoType::Int32 | ProtoType::Sint32 | ProtoType::Sfixed32 => CliParam {
            rust_type: "i32".to_string(),
            rendering: CliRendering::Flag,
        },
        ProtoType::Bool => CliParam {
            rust_type: "bool".to_string(),
            rendering: CliRendering::BoolFlag,
        },
        ProtoType::Bytes => CliParam {
            rust_type: "&str".to_string(), // hex or base64
            rendering: CliRendering::Flag,
        },
        ProtoType::Message => {
            // Special-case known Cosmos types
            if type_name.ends_with(".Coin") || type_name.ends_with("Coin") {
                CliParam {
                    rust_type: "&str".to_string(), // "100utoken" format
                    rendering: CliRendering::Flag,
                }
            } else {
                // Generic message → serialize as JSON string
                CliParam {
                    rust_type: "&str".to_string(),
                    rendering: CliRendering::Flag,
                }
            }
        }
        // Enums, doubles, floats → string representation
        _ => CliParam {
            rust_type: "&str".to_string(),
            rendering: CliRendering::Flag,
        },
    }
}

/// Check if a field name is a sender/signer field.
pub fn is_sender_field(name: &str) -> bool {
    matches!(
        name,
        "sender" | "authority" | "creator" | "admin" | "from_address" | "signer"
    )
}
