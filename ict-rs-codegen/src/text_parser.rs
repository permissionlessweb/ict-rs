//! Regex-based proto file parser.
//!
//! Reads `.proto` text files directly and extracts service/RPC/message
//! definitions without requiring `protoc` to be installed.

use prost_types::field_descriptor_proto::Type as ProtoType;
use regex::Regex;

use crate::proto_parser::{ParsedField, ParsedRpc, ParsedService};

/// Parse a single `.proto` file's text content.
///
/// Returns all `Msg` and `Query` services found in the file, with their
/// RPCs and input message fields resolved.
pub fn parse_proto_text(content: &str) -> Vec<ParsedService> {
    let package = parse_package(content);
    let messages = parse_messages(content);
    let services = parse_services(content);

    let mut result = Vec::new();

    for (svc_name, rpc_defs) in services {
        if svc_name != "Msg" && svc_name != "Query" {
            continue;
        }

        let rpcs = rpc_defs
            .into_iter()
            .filter(|r| !r.0.ends_with("Response"))
            .filter_map(|(rpc_name, input_type, output_type)| {
                let input_fields = messages
                    .iter()
                    .find(|m| m.0 == input_type)
                    .map(|m| m.1.clone())
                    .unwrap_or_default();

                let signer_field = detect_signer_from_text(content, &input_type)
                    .or_else(|| detect_signer_from_fields(&input_fields));

                Some(ParsedRpc {
                    name: rpc_name,
                    input_type: format!(".{}.{}", package, input_type),
                    output_type: format!(".{}.{}", package, output_type),
                    input_fields,
                    signer_field,
                })
            })
            .collect();

        result.push(ParsedService {
            name: svc_name,
            package: package.clone(),
            rpcs,
        });
    }

    result
}

/// Extract the package name from `package foo.bar.v1;`
fn parse_package(content: &str) -> String {
    let re = Regex::new(r"(?m)^package\s+([\w.]+)\s*;").unwrap();
    re.captures(content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// Extract all message definitions: `(name, fields)`.
fn parse_messages(content: &str) -> Vec<(String, Vec<ParsedField>)> {
    let mut result = Vec::new();

    // Match `message Name { ... }` — handles nested braces by counting
    let msg_re = Regex::new(r"(?m)^message\s+(\w+)\s*\{").unwrap();

    for cap in msg_re.captures_iter(content) {
        let name = cap[1].to_string();
        let start = cap.get(0).unwrap().end();

        if let Some(body) = extract_brace_body(content, start) {
            let fields = parse_message_fields(&body);
            result.push((name, fields));
        }
    }

    result
}

/// Extract the body between matched braces starting after `{`.
fn extract_brace_body(content: &str, start: usize) -> Option<String> {
    let bytes = content.as_bytes();
    let mut depth = 1;
    let mut pos = start;

    while pos < bytes.len() && depth > 0 {
        match bytes[pos] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            pos += 1;
        }
    }

    if depth == 0 {
        Some(content[start..pos].to_string())
    } else {
        None
    }
}

/// Parse fields from a message body.
///
/// Handles patterns like:
/// - `string sender = 1;`
/// - `string sender = 1 [ ... ];`
/// - `repeated string denoms = 1;`
/// - `cosmos.base.v1beta1.Coin amount = 2 [ ... ];`
fn parse_message_fields(body: &str) -> Vec<ParsedField> {
    let field_re = Regex::new(
        r"(?m)^\s*(repeated\s+)?([\w.]+)\s+(\w+)\s*=\s*\d+",
    )
    .unwrap();

    let mut fields = Vec::new();

    for cap in field_re.captures_iter(body) {
        let is_repeated = cap.get(1).is_some();
        let type_str = cap[2].to_string();
        let field_name = cap[3].to_string();

        // Skip nested message/enum/option lines
        if field_name == "option" || type_str == "option" || type_str == "reserved" {
            continue;
        }

        let (proto_type, type_name) = proto_type_from_string(&type_str);

        fields.push(ParsedField {
            name: field_name,
            proto_type,
            type_name,
            is_repeated,
        });
    }

    fields
}

/// Map a proto type string to `ProtoType` enum + type_name.
fn proto_type_from_string(s: &str) -> (ProtoType, String) {
    match s {
        "string" => (ProtoType::String, String::new()),
        "bytes" => (ProtoType::Bytes, String::new()),
        "bool" => (ProtoType::Bool, String::new()),
        "int32" => (ProtoType::Int32, String::new()),
        "int64" => (ProtoType::Int64, String::new()),
        "uint32" => (ProtoType::Uint32, String::new()),
        "uint64" => (ProtoType::Uint64, String::new()),
        "sint32" => (ProtoType::Sint32, String::new()),
        "sint64" => (ProtoType::Sint64, String::new()),
        "fixed32" => (ProtoType::Fixed32, String::new()),
        "fixed64" => (ProtoType::Fixed64, String::new()),
        "sfixed32" => (ProtoType::Sfixed32, String::new()),
        "sfixed64" => (ProtoType::Sfixed64, String::new()),
        "float" => (ProtoType::Float, String::new()),
        "double" => (ProtoType::Double, String::new()),
        // Anything else is a message type reference
        other => (ProtoType::Message, other.to_string()),
    }
}

/// Parse all service definitions: `(service_name, vec of (rpc_name, input_type, output_type))`.
fn parse_services(content: &str) -> Vec<(String, Vec<(String, String, String)>)> {
    let svc_re = Regex::new(r"(?m)^service\s+(\w+)\s*\{").unwrap();
    let mut result = Vec::new();

    for cap in svc_re.captures_iter(content) {
        let name = cap[1].to_string();
        let start = cap.get(0).unwrap().end();

        if let Some(body) = extract_brace_body(content, start) {
            let rpcs = parse_rpcs(&body);
            result.push((name, rpcs));
        }
    }

    result
}

/// Parse RPC definitions from a service body.
fn parse_rpcs(body: &str) -> Vec<(String, String, String)> {
    let rpc_re = Regex::new(
        r"rpc\s+(\w+)\s*\(\s*(\w+)\s*\)\s*returns\s*\(\s*(\w+)\s*\)",
    )
    .unwrap();

    rpc_re
        .captures_iter(body)
        .map(|cap| (cap[1].to_string(), cap[2].to_string(), cap[3].to_string()))
        .collect()
}

/// Detect signer from `option (cosmos.msg.v1.signer) = "sender";` inside a message.
fn detect_signer_from_text(content: &str, message_name: &str) -> Option<String> {
    // Find the message body
    let msg_re = Regex::new(&format!(r"message\s+{}\s*\{{", regex::escape(message_name))).unwrap();
    let cap = msg_re.find(content)?;
    let body = extract_brace_body(content, cap.end())?;

    let signer_re =
        Regex::new(r#"option\s+\(cosmos\.msg\.v1\.signer\)\s*=\s*"(\w+)""#).unwrap();
    signer_re
        .captures(&body)
        .map(|c| c[1].to_string())
}

/// Fallback signer detection from field names.
fn detect_signer_from_fields(fields: &[ParsedField]) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENFACTORY_TX: &str = r#"
syntax = "proto3";
package osmosis.tokenfactory.v1beta1;

import "cosmos/msg/v1/msg.proto";

service Msg {
  option (cosmos.msg.v1.service) = true;
  rpc CreateDenom(MsgCreateDenom) returns (MsgCreateDenomResponse);
  rpc Mint(MsgMint) returns (MsgMintResponse);
  rpc Burn(MsgBurn) returns (MsgBurnResponse);
  rpc ChangeAdmin(MsgChangeAdmin) returns (MsgChangeAdminResponse);
}

message MsgCreateDenom {
  option (cosmos.msg.v1.signer) = "sender";
  string sender = 1;
  string subdenom = 2;
}

message MsgCreateDenomResponse {
  string new_token_denom = 1;
}

message MsgMint {
  option (cosmos.msg.v1.signer) = "sender";
  string sender = 1;
  cosmos.base.v1beta1.Coin amount = 2;
  string mintToAddress = 3;
}

message MsgMintResponse {}

message MsgBurn {
  option (cosmos.msg.v1.signer) = "sender";
  string sender = 1;
  cosmos.base.v1beta1.Coin amount = 2;
  string burnFromAddress = 3;
}

message MsgBurnResponse {}

message MsgChangeAdmin {
  option (cosmos.msg.v1.signer) = "sender";
  string sender = 1;
  string denom = 2;
  string new_admin = 3;
}

message MsgChangeAdminResponse {}
"#;

    const TOKENFACTORY_QUERY: &str = r#"
syntax = "proto3";
package osmosis.tokenfactory.v1beta1;

service Query {
  rpc Params(QueryParamsRequest) returns (QueryParamsResponse);
  rpc DenomAuthorityMetadata(QueryDenomAuthorityMetadataRequest) returns (QueryDenomAuthorityMetadataResponse);
  rpc DenomsFromCreator(QueryDenomsFromCreatorRequest) returns (QueryDenomsFromCreatorResponse);
}

message QueryParamsRequest {}

message QueryParamsResponse {
  Params params = 1;
}

message QueryDenomAuthorityMetadataRequest {
  string denom = 1;
}

message QueryDenomAuthorityMetadataResponse {
  DenomAuthorityMetadata authority_metadata = 1;
}

message QueryDenomsFromCreatorRequest {
  string creator = 1;
}

message QueryDenomsFromCreatorResponse {
  repeated string denoms = 1;
}
"#;

    #[test]
    fn test_parse_package() {
        assert_eq!(
            parse_package(TOKENFACTORY_TX),
            "osmosis.tokenfactory.v1beta1"
        );
    }

    #[test]
    fn test_parse_messages() {
        let messages = parse_messages(TOKENFACTORY_TX);
        let names: Vec<&str> = messages.iter().map(|m| m.0.as_str()).collect();
        assert!(names.contains(&"MsgCreateDenom"));
        assert!(names.contains(&"MsgMint"));
        assert!(names.contains(&"MsgBurn"));
        assert!(names.contains(&"MsgChangeAdmin"));

        // Check MsgCreateDenom fields
        let create_denom = messages.iter().find(|m| m.0 == "MsgCreateDenom").unwrap();
        assert_eq!(create_denom.1.len(), 2);
        assert_eq!(create_denom.1[0].name, "sender");
        assert_eq!(create_denom.1[1].name, "subdenom");

        // Check MsgMint fields — has Coin type
        let mint = messages.iter().find(|m| m.0 == "MsgMint").unwrap();
        assert_eq!(mint.1.len(), 3);
        assert_eq!(mint.1[1].name, "amount");
        assert_eq!(mint.1[1].proto_type, ProtoType::Message);
        assert!(mint.1[1].type_name.contains("Coin"));
    }

    #[test]
    fn test_parse_services_msg() {
        let services = parse_proto_text(TOKENFACTORY_TX);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "Msg");
        assert_eq!(services[0].package, "osmosis.tokenfactory.v1beta1");
        assert_eq!(services[0].rpcs.len(), 4);

        let create = &services[0].rpcs[0];
        assert_eq!(create.name, "CreateDenom");
        assert_eq!(create.signer_field.as_deref(), Some("sender"));
        assert_eq!(create.input_fields.len(), 2);
    }

    #[test]
    fn test_parse_services_query() {
        let services = parse_proto_text(TOKENFACTORY_QUERY);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "Query");
        assert_eq!(services[0].rpcs.len(), 3);

        let denom_auth = &services[0].rpcs[1];
        assert_eq!(denom_auth.name, "DenomAuthorityMetadata");
        assert_eq!(denom_auth.input_fields.len(), 1);
        assert_eq!(denom_auth.input_fields[0].name, "denom");
    }

    #[test]
    fn test_signer_detection() {
        let services = parse_proto_text(TOKENFACTORY_TX);
        for rpc in &services[0].rpcs {
            assert_eq!(rpc.signer_field.as_deref(), Some("sender"));
        }
    }

    #[test]
    fn test_empty_message_fields() {
        let services = parse_proto_text(TOKENFACTORY_QUERY);
        let params = &services[0].rpcs[0];
        assert_eq!(params.name, "Params");
        assert_eq!(params.input_fields.len(), 0);
    }
}
