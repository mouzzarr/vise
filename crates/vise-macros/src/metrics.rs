//! Derivation of the `Metrics` trait.

use proc_macro::TokenStream;
use quote::{quote, quote_spanned};
use syn::{Attribute, Data, DeriveInput, Expr, Field, Ident, Lit, Path};

use std::fmt;

use crate::utils::{metrics_attribute, validate_name, ParseAttribute};

/// Struct-level `#[metrics(..)]` attributes.
#[derive(Default)]
struct MetricsAttrs {
    cr: Option<Path>,
    prefix: String,
}

impl fmt::Debug for MetricsAttrs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetricsAttrs")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl ParseAttribute for MetricsAttrs {
    fn parse(raw: &Attribute) -> syn::Result<Self> {
        let mut attrs = Self::default();
        raw.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                attrs.cr = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("prefix") {
                let prefix_str: syn::LitStr = meta.value()?.parse()?;
                attrs.prefix = prefix_str.value();
                validate_name(&attrs.prefix).map_err(|message| meta.error(message))
            } else {
                Err(meta.error("unsupported attribute"))
            }
        })?;
        Ok(attrs)
    }
}

#[derive(Default)]
struct MetricsFieldAttrs {
    buckets: Option<Expr>,
    unit: Option<Expr>,
}

impl fmt::Debug for MetricsFieldAttrs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetricsFieldAttrs")
            .field("buckets", &self.buckets.as_ref().map(|_| ".."))
            .field("unit", &self.unit.as_ref().map(|_| ".."))
            .finish()
    }
}

impl ParseAttribute for MetricsFieldAttrs {
    fn parse(raw: &Attribute) -> syn::Result<Self> {
        let mut attrs = Self::default();
        raw.parse_nested_meta(|meta| {
            if meta.path.is_ident("buckets") {
                attrs.buckets = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("unit") {
                attrs.unit = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("unsupported attribute"))
            }
        })?;
        Ok(attrs)
    }
}

#[derive(Debug)]
struct MetricsField {
    attrs: MetricsFieldAttrs,
    name: Ident,
    docs: String,
}

impl MetricsField {
    fn parse(raw: &Field) -> syn::Result<Self> {
        let name = raw.ident.clone().ok_or_else(|| {
            let message = "Only named fields are supported";
            syn::Error::new_spanned(raw, message)
        })?;
        validate_name(&name.to_string())
            .map_err(|message| syn::Error::new(name.span(), message))?;

        let attrs = metrics_attribute(&raw.attrs)?;

        let doc_lines = raw.attrs.iter().filter_map(|attr| {
            if attr.meta.path().is_ident("doc") {
                let name_value = attr.meta.require_name_value().ok()?;
                let Expr::Lit(doc_literal) = &name_value.value else {
                    return None;
                };
                match &doc_literal.lit {
                    Lit::Str(doc_literal) => Some(doc_literal.value()),
                    _ => None,
                }
            } else {
                None
            }
        });

        let mut docs = String::new();
        for line in doc_lines {
            let line = line.trim();
            if !line.is_empty() {
                if !docs.is_empty() {
                    docs.push(' ');
                }
                docs.push_str(line);
            }
        }
        if docs.ends_with(|ch: char| ch == '.' || ch == '!' || ch == '?') {
            // Remove the trailing punctuation since it'll be inserted automatically by the `Registry`.
            docs.pop();
        }

        Ok(Self { attrs, name, docs })
    }

    fn initialize_default(&self, cr: &proc_macro2::TokenStream) -> proc_macro2::TokenStream {
        let name = &self.name;
        let constructor = if let Some(buckets) = &self.attrs.buckets {
            quote!(<#cr::Buckets as core::convert::From<_>>::from(#buckets))
        } else {
            quote!(#cr::DefaultConstructor)
        };
        quote_spanned! {name.span()=>
            #name: #cr::ConstructMetric::construct(&#constructor)
        }
    }

    fn register(&self, prefix: Option<&str>) -> proc_macro2::TokenStream {
        let name = &self.name;
        let name_str = if let Some(prefix) = prefix {
            format!("{prefix}_{name}")
        } else {
            name.to_string()
        };
        let docs = &self.docs;

        let unit = if let Some(unit) = &self.attrs.unit {
            quote!(core::option::Option::Some(#unit))
        } else {
            quote!(core::option::Option::None)
        };

        quote! {
            __registration.register(
                #name_str,
                #docs,
                #unit,
                core::clone::Clone::clone(&self.#name),
            );
        }
    }
}

#[derive(Debug)]
struct MetricsImpl {
    attrs: MetricsAttrs,
    name: Ident,
    fields: Vec<MetricsField>,
}

impl MetricsImpl {
    fn new(input: &DeriveInput) -> syn::Result<Self> {
        let Data::Struct(data) = &input.data else {
            let message = "#[derive(Metrics)] can only be placed on structs";
            return Err(syn::Error::new_spanned(input, message));
        };

        let attrs = metrics_attribute(&input.attrs)?;
        let name = input.ident.clone();
        let fields = data.fields.iter().map(MetricsField::parse);
        let fields = fields.collect::<syn::Result<_>>()?;
        Ok(Self {
            attrs,
            name,
            fields,
        })
    }

    fn path_to_crate(&self) -> proc_macro2::TokenStream {
        if let Some(cr) = &self.attrs.cr {
            quote!(#cr)
        } else {
            quote!(vise)
        }
    }

    fn initialize(&self) -> proc_macro2::TokenStream {
        let cr = self.path_to_crate();
        let fields = self
            .fields
            .iter()
            .map(|field| field.initialize_default(&cr));

        quote! {
            Self {
                #(#fields,)*
            }
        }
    }

    fn implement_metrics(&self) -> proc_macro2::TokenStream {
        let cr = self.path_to_crate();
        let name = &self.name;
        let prefix = self.attrs.prefix.as_str();
        let prefix = (!prefix.is_empty()).then_some(prefix);
        let register_fields = self.fields.iter().map(|field| field.register(prefix));

        quote! {
            impl #cr::Metrics for #name {
                fn register_metrics(&self, __registry: &mut #cr::Registry) {
                    let mut __registration = __registry.register_metrics();
                    #(#register_fields;)*
                }
            }
        }
    }

    fn derive_traits(&self) -> proc_macro2::TokenStream {
        let cr = self.path_to_crate();
        let name = &self.name;
        let initialization = self.initialize();
        let default_impl = quote! {
            impl core::default::Default for #name {
                fn default() -> Self {
                    #initialization
                }
            }
        };
        let metrics_impl = self.implement_metrics();

        quote! {
            #default_impl
            #metrics_impl

            const _: () = {
                impl #name {
                    /// Returns the instance of this metric used for metrics recording.
                    pub fn instance() -> &'static Self {
                        static THIS: #cr::_reexports::Lazy<#name> =
                            #cr::_reexports::Lazy::new(core::default::Default::default);
                        &*THIS
                    }
                }

                #[#cr::_reexports::linkme::distributed_slice(#cr::METRICS_REGISTRATIONS)]
                #[linkme(crate = #cr::_reexports::linkme)]
                fn __register_test_metrics(registry: &mut #cr::Registry) {
                    let instance = #name::instance();
                    #cr::Metrics::register_metrics(instance, registry);
                }
            };
        }
    }
}

pub(crate) fn impl_metrics(input: TokenStream) -> TokenStream {
    let input: DeriveInput = syn::parse(input).unwrap();
    let trait_impl = match MetricsImpl::new(&input) {
        Ok(trait_impl) => trait_impl,
        Err(err) => return err.into_compile_error().into(),
    };
    trait_impl.derive_traits().into()
}
