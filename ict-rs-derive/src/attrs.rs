//! Attribute parsing for `#[ict(...)]`, `#[returns(...)]`.

use syn::{Attribute, Meta};

/// Container-level attributes parsed from `#[ict(module = "name")]`.
pub struct ContainerAttrs {
    /// The CLI module name (e.g. "tokenfactory", "bank").
    pub module: String,
}

impl ContainerAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> Self {
        let mut module = None;

        for attr in attrs {
            if !attr.path().is_ident("ict") {
                continue;
            }
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("module") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    module = Some(lit.value());
                }
                Ok(())
            });
        }

        ContainerAttrs {
            module: module.unwrap_or_default(),
        }
    }
}

/// Field-level attributes from `#[ict(sender)]` and `#[ict(skip)]`.
pub struct FieldAttrs {
    pub is_sender: bool,
}

impl FieldAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> Self {
        let mut is_sender = false;

        for attr in attrs {
            if !attr.path().is_ident("ict") {
                continue;
            }
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("sender") {
                    is_sender = true;
                }
                Ok(())
            });
        }

        FieldAttrs { is_sender }
    }
}

/// Variant-level attributes: `#[ict(skip)]`.
pub struct VariantAttrs {
    pub skip: bool,
}

impl VariantAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> Self {
        let mut skip = false;

        for attr in attrs {
            if !attr.path().is_ident("ict") {
                continue;
            }
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip") {
                    skip = true;
                }
                Ok(())
            });
        }

        VariantAttrs { skip }
    }
}

/// Parse `#[returns(Type)]` from variant attributes.
pub fn parse_returns_attr(attrs: &[Attribute]) -> Option<syn::Type> {
    for attr in attrs {
        if !attr.path().is_ident("returns") {
            continue;
        }
        // #[returns(SomeType)]
        if let Meta::List(list) = &attr.meta {
            if let Ok(ty) = syn::parse2::<syn::Type>(list.tokens.clone()) {
                return Some(ty);
            }
        }
    }
    None
}

/// Check if a field name looks like a sender field.
pub fn is_sender_field_name(name: &str) -> bool {
    matches!(name, "sender" | "authority" | "creator" | "admin" | "from_address")
}
