//! Generate query trait code from parsed service definitions.
//!
//! Query fields become **positional** CLI args (in proto field order),
//! matching Cosmos SDK CLI conventions.

use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::naming;
use crate::proto_parser::ParsedService;

/// Generate a Rust source string containing the query extension trait.
///
/// Uses `crate::` paths (for code checked into the ict-rs crate).
pub fn generate_query_trait(service: &ParsedService) -> String {
    generate_query_trait_with_path(service, "crate")
}

/// Generate with an explicit crate path prefix.
pub fn generate_query_trait_with_path(service: &ParsedService, crate_path: &str) -> String {
    let module = naming::module_from_package(&service.package);
    let trait_name = format_ident!("{}", naming::query_trait_name(&module));
    let krate: TokenStream = crate_path.parse().unwrap();

    let mut methods = Vec::new();

    for rpc in &service.rpcs {
        let method_name = format_ident!("{}", naming::method_name(&module, &rpc.name));
        let action_kebab = naming::cli_action(&rpc.name);

        let param_decls: Vec<TokenStream> = rpc
            .input_fields
            .iter()
            .map(|f| {
                let name = format_ident!("{}", f.name.to_snake_case());
                quote! { #name: &str }
            })
            .collect();

        // All query fields are positional args
        let field_pushes: Vec<TokenStream> = rpc
            .input_fields
            .iter()
            .map(|f| {
                let name = format_ident!("{}", f.name.to_snake_case());
                quote! {
                    args.push(#name.to_string());
                }
            })
            .collect();

        let module_str = &module;
        methods.push(quote! {
            async fn #method_name(
                &self, #(#param_decls),*
            ) -> #krate::error::Result<serde_json::Value> {
                let mut args: Vec<String> = vec![
                    "query".to_string(),
                    #module_str.to_string(),
                    #action_kebab.to_string(),
                ];
                #(#field_pushes)*
                for flag in #krate::cli::QUERY_DEFAULT_FLAGS {
                    args.push(flag.to_string());
                }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let output = self.exec(&arg_refs, &[]).await?;
                #krate::cli::parse_query_response(&output)
            }
        });
    }

    let tokens = quote! {
        #[#krate::cli::async_trait]
        pub trait #trait_name: #krate::chain::Chain {
            #(#methods)*
        }

        impl<T: #krate::chain::Chain + ?Sized> #trait_name for T {}
    };

    tokens.to_string()
}
