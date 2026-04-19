//! Implementation of the `QueryFns` derive macro.
//!
//! Generates an async extension trait on `ict_rs::chain::Chain` with one method
//! per enum variant. Each method composes a CLI `query` command and parses the
//! JSON response.

use heck::{ToKebabCase, ToSnakeCase};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields};

use crate::attrs::{ContainerAttrs, VariantAttrs};

pub fn expand(input: &DeriveInput) -> TokenStream {
    let container = ContainerAttrs::from_attrs(&input.attrs);
    let module = &container.module;

    if module.is_empty() {
        return syn::Error::new_spanned(
            &input.ident,
            "QueryFns requires #[ict(module = \"name\")] on the enum",
        )
        .to_compile_error();
    }

    let enum_data = match &input.data {
        Data::Enum(e) => e,
        _ => {
            return syn::Error::new_spanned(&input.ident, "QueryFns can only be derived on enums")
                .to_compile_error();
        }
    };

    let trait_name = format_ident!("{}QueryExt", heck::AsUpperCamelCase(module).to_string());
    let mut methods = Vec::new();

    for variant in &enum_data.variants {
        let vattrs = VariantAttrs::from_attrs(&variant.attrs);
        if vattrs.skip {
            continue;
        }

        let variant_name = &variant.ident;
        let action_snake = variant_name.to_string().to_snake_case();
        let method_name = format_ident!("{}_{}", module, action_snake);
        let action_kebab = variant_name.to_string().to_kebab_case();

        let fields = match &variant.fields {
            Fields::Named(f) => &f.named,
            Fields::Unit => {
                // Unit variant → no params
                methods.push(quote! {
                    async fn #method_name(
                        &self,
                    ) -> ict_rs::error::Result<serde_json::Value> {
                        let mut args: Vec<String> = vec![
                            "query".to_string(),
                            #module.to_string(),
                            #action_kebab.to_string(),
                        ];
                        for flag in ict_rs::cli::QUERY_DEFAULT_FLAGS {
                            args.push(flag.to_string());
                        }
                        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        let output = self.exec(&arg_refs, &[]).await?;
                        ict_rs::cli::parse_query_response(&output)
                    }
                });
                continue;
            }
            Fields::Unnamed(_) => {
                continue;
            }
        };

        let param_names: Vec<_> = fields
            .iter()
            .map(|f| f.ident.as_ref().unwrap().clone())
            .collect();

        let param_decls: Vec<_> = param_names
            .iter()
            .map(|name| quote! { #name: &str })
            .collect();

        let field_pushes: Vec<_> = param_names
            .iter()
            .map(|f| {
                let flag = format!("--{}", f.to_string().to_kebab_case());
                quote! {
                    args.push(#flag.to_string());
                    args.push(#f.to_string());
                }
            })
            .collect();

        methods.push(quote! {
            async fn #method_name(
                &self, #(#param_decls),*
            ) -> ict_rs::error::Result<serde_json::Value> {
                let mut args: Vec<String> = vec![
                    "query".to_string(),
                    #module.to_string(),
                    #action_kebab.to_string(),
                ];
                #(#field_pushes)*
                for flag in ict_rs::cli::QUERY_DEFAULT_FLAGS {
                    args.push(flag.to_string());
                }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let output = self.exec(&arg_refs, &[]).await?;
                ict_rs::cli::parse_query_response(&output)
            }
        });
    }

    quote! {
        #[ict_rs::cli::async_trait]
        pub trait #trait_name: ict_rs::chain::Chain {
            #(#methods)*
        }

        impl<T: ict_rs::chain::Chain + ?Sized> #trait_name for T {}
    }
}
