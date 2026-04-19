//! Extract service and RPC definitions from a prost `FileDescriptorSet`.

use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorSet, MethodDescriptorProto,
    field_descriptor_proto::Type as ProtoType,
};

/// A parsed service definition (Msg or Query).
#[derive(Debug, Clone)]
pub struct ParsedService {
    /// Service name: "Msg" or "Query"
    pub name: String,
    /// Full proto package (e.g. "osmosis.tokenfactory.v1beta1")
    pub package: String,
    /// RPC methods defined on this service
    pub rpcs: Vec<ParsedRpc>,
}

/// A parsed RPC method.
#[derive(Debug, Clone)]
pub struct ParsedRpc {
    /// Method name (e.g. "CreateDenom")
    pub name: String,
    /// Input message type (fully qualified)
    pub input_type: String,
    /// Output message type (fully qualified)
    pub output_type: String,
    /// Fields of the input message
    pub input_fields: Vec<ParsedField>,
    /// The signer field name, if detected
    pub signer_field: Option<String>,
}

/// A parsed message field.
#[derive(Debug, Clone)]
pub struct ParsedField {
    pub name: String,
    pub proto_type: ProtoType,
    /// For message-type fields, the fully qualified type name
    pub type_name: String,
    pub is_repeated: bool,
}

/// Extract all Msg and Query services from a FileDescriptorSet.
pub fn extract_services(fds: &FileDescriptorSet) -> Vec<ParsedService> {
    let mut services = Vec::new();

    for file in &fds.file {
        let package = file.package.clone().unwrap_or_default();

        // Collect all message definitions for field lookup
        let messages: Vec<&DescriptorProto> = file.message_type.iter().collect();

        for service in &file.service {
            let svc_name = service.name.clone().unwrap_or_default();
            if svc_name != "Msg" && svc_name != "Query" {
                continue;
            }

            let rpcs = service
                .method
                .iter()
                .filter_map(|method| parse_rpc(method, &messages, &package))
                .collect();

            services.push(ParsedService {
                name: svc_name,
                package: package.clone(),
                rpcs,
            });
        }
    }

    services
}

fn parse_rpc(
    method: &MethodDescriptorProto,
    messages: &[&DescriptorProto],
    _package: &str,
) -> Option<ParsedRpc> {
    let name = method.name.clone()?;
    let input_type = method.input_type.clone().unwrap_or_default();
    let output_type = method.output_type.clone().unwrap_or_default();

    // Skip response-type methods (MsgXxxResponse)
    if name.ends_with("Response") {
        return None;
    }

    // Find the input message definition to extract fields
    let input_msg_name = input_type
        .rsplit('.')
        .next()
        .unwrap_or(&input_type);

    let input_fields = messages
        .iter()
        .find(|m| m.name.as_deref() == Some(input_msg_name))
        .map(|m| parse_fields(&m.field))
        .unwrap_or_default();

    // Detect signer field
    let signer_field = detect_signer(&input_fields);

    Some(ParsedRpc {
        name,
        input_type,
        output_type,
        input_fields,
        signer_field,
    })
}

fn parse_fields(fields: &[FieldDescriptorProto]) -> Vec<ParsedField> {
    fields
        .iter()
        .filter_map(|f| {
            let name = f.name.clone()?;
            let proto_type = f.r#type().into();
            let type_name = f.type_name.clone().unwrap_or_default();
            let is_repeated = f.label() == prost_types::field_descriptor_proto::Label::Repeated;

            Some(ParsedField {
                name,
                proto_type,
                type_name,
                is_repeated,
            })
        })
        .collect()
}

fn detect_signer(fields: &[ParsedField]) -> Option<String> {
    // Look for common signer field names
    for field in fields {
        if matches!(
            field.name.as_str(),
            "sender" | "authority" | "creator" | "admin" | "from_address" | "signer"
        ) {
            return Some(field.name.clone());
        }
    }
    None
}
