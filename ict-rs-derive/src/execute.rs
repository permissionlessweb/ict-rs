//! Implementation of the `ExecuteFns` derive macro.
//!
//! Generates an async extension trait on `ict_rs::chain::Chain` with one method
//! per enum variant. Each method composes a CLI `tx` command and parses the
//! response into a `Tx`.

use heck::{ToKebabCase, ToSnakeCase};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields};

use crate::attrs::{is_sender_field_name, ContainerAttrs, FieldAttrs, VariantAttrs};

pub fn expand(input: &DeriveInput) -> TokenStream {
    let container = ContainerAttrs::from_attrs(&input.attrs);
    let module = &container.module;

    if module.is_empty() {
        return syn::Error::new_spanned(
            &input.ident,
            "ExecuteFns requires #[ict(module = \"name\")] on the enum",
        )
        .to_compile_error();
    }

    let enum_data = match &input.data {
        Data::Enum(e) => e,
        _ => {
            return syn::Error::new_spanned(&input.ident, "ExecuteFns can only be derived on enums")
                .to_compile_error();
        }
    };

    let trait_name = format_ident!("{}MsgExt", heck::AsUpperCamelCase(module).to_string());
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
                // Unit variant → no params besides key_name
                methods.push(quote! {
                    async fn #method_name(
                        &self, key_name: &str,
                    ) -> ict_rs::error::Result<ict_rs::tx::Tx> {
                        let gas_prices = self.config().gas_prices.clone();
                        let chain_id = self.chain_id().to_string();
                        let mut args: Vec<String> = vec![
                            "tx".to_string(),
                            #module.to_string(),
                            #action_kebab.to_string(),
                        ];
                        args.extend([
                            "--from".to_string(), key_name.to_string(),
                            "--gas-prices".to_string(), gas_prices,
                            "--chain-id".to_string(), chain_id,
                        ]);
                        for flag in ict_rs::cli::TX_DEFAULT_FLAGS {
                            args.push(flag.to_string());
                        }
                        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        let output = self.exec(&arg_refs, &[]).await?;
                        ict_rs::cli::parse_tx_response(&output)
                    }
                });
                continue;
            }
            Fields::Unnamed(_) => {
                continue; // skip tuple variants
            }
        };

        // Separate sender field from regular fields
        let mut sender_field = None;
        let mut regular_fields = Vec::new();

        for field in fields {
            let field_name = field.ident.as_ref().unwrap();
            let fattrs = FieldAttrs::from_attrs(&field.attrs);

            if fattrs.is_sender || (sender_field.is_none() && is_sender_field_name(&field_name.to_string())) {
                sender_field = Some(field_name.clone());
            } else {
                regular_fields.push(field_name.clone());
            }
        }

        // Build method params: key_name first, then regular fields as &str
        let param_names: Vec<_> = regular_fields.iter().map(|f| {
            format_ident!("{}", f)
        }).collect();

        let param_decls: Vec<_> = param_names.iter().map(|name| {
            quote! { #name: &str }
        }).collect();

        // Build CLI args: positional fields first, then --from key_name
        let field_pushes: Vec<_> = regular_fields.iter().map(|f| {
            let flag = format!("--{}", f.to_string().to_kebab_case());
            let ident = format_ident!("{}", f);
            quote! {
                args.push(#flag.to_string());
                args.push(#ident.to_string());
            }
        }).collect();

        methods.push(quote! {
            async fn #method_name(
                &self, key_name: &str, #(#param_decls),*
            ) -> ict_rs::error::Result<ict_rs::tx::Tx> {
                let gas_prices = self.config().gas_prices.clone();
                let chain_id = self.chain_id().to_string();
                let mut args: Vec<String> = vec![
                    "tx".to_string(),
                    #module.to_string(),
                    #action_kebab.to_string(),
                ];
                #(#field_pushes)*
                args.extend([
                    "--from".to_string(), key_name.to_string(),
                    "--gas-prices".to_string(), gas_prices,
                    "--chain-id".to_string(), chain_id,
                ]);
                for flag in ict_rs::cli::TX_DEFAULT_FLAGS {
                    args.push(flag.to_string());
                }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let output = self.exec(&arg_refs, &[]).await?;
                ict_rs::cli::parse_tx_response(&output)
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
