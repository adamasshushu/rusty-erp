//! `#[derive(Doctype)]` proc macro
//!

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Lit, Meta};

/// 解析 `#[doctype(name = "...", table = "...")]` 属性
struct DoctypeAttr {
    name: String,
    table_name: String,
    is_single: bool,
    autoname: Option<String>,
}

fn parse_doctype_attr(attrs: &[syn::Attribute]) -> DoctypeAttr {
    let mut name = String::new();
    let mut table_name = String::new();
    let mut is_single = false;
    let mut autoname = None;

    for attr in attrs {
        if !attr.path().is_ident("doctype") {
            continue;
        }

        // Parse #[doctype(name = "x", table = "y")]
        if let Meta::List(list) = &attr.meta {
            let _ = list.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let val: Lit = meta.value()?.parse()?;
                    if let Lit::Str(s) = val {
                        name = s.value();
                    }
                } else if meta.path.is_ident("table") {
                    let val: Lit = meta.value()?.parse()?;
                    if let Lit::Str(s) = val {
                        table_name = s.value();
                    }
                } else if meta.path.is_ident("single") {
                    is_single = true;
                } else if meta.path.is_ident("autoname") {
                    let val: Lit = meta.value()?.parse()?;
                    if let Lit::Str(s) = val {
                        autoname = Some(s.value());
                    }
                } else {
                    // Ignore field-level attributes like #[doctype(field = "...")]
                }
                Ok(())
            });
        }
    }

    DoctypeAttr {
        name: name.clone(),
        table_name: if table_name.is_empty() {
            format!("tab{}", name)
        } else {
            table_name
        },
        is_single,
        autoname,
    }
}

#[proc_macro_derive(Doctype, attributes(doctype))]
pub fn derive_doctype(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;
    let _struct_name_str = struct_name.to_string();

    let attr = parse_doctype_attr(&input.attrs);
    let name_lit = &attr.name;
    let table_lit = &attr.table_name;
    let is_single = attr.is_single;
    let autoname = attr.autoname.as_deref().unwrap_or("");

    // Generate trait implementation
    let expanded = quote! {
        impl frappe_core::Doctype for #struct_name {
            fn meta() -> &'static frappe_core::DoctypeMeta {
                static META: std::sync::LazyLock<frappe_core::DoctypeMeta> =
                    std::sync::LazyLock::new(|| frappe_core::DoctypeMeta {
                        name: #name_lit,
                        table_name: #table_lit,
                        is_single: #is_single,
                        has_children: false,
                        autoname: if #autoname.is_empty() { None } else { Some(#autoname) },
                    });
                &META
            }

            fn name(&self) -> &str {
                &self.name
            }

            fn docstatus(&self) -> frappe_core::DocStatus {
                frappe_core::DocStatus::from_int(self.docstatus)
            }

            fn validate(&self) -> Result<(), Vec<String>> {
                let mut errors: Vec<String> = Vec::new();
                // Auto-validate required fields (compile-time checked via build.rs)
                // Hook: user-defined validate methods
                let _ = self;
                if errors.is_empty() { Ok(()) } else { Err(errors) }
            }

            fn on_submit(&mut self) {
                self.docstatus = 1;
            }

            fn on_cancel(&mut self) {
                self.docstatus = 2;
            }

            // before_save / before_insert / before_submit / before_cancel
            // — 默认空实现，通过 trait default methods 提供
        }
    };

    TokenStream::from(expanded)
}
