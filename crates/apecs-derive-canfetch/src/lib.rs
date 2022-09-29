//! This library is meant to be used to create `CanFetch` derive macros for the
//! `apecs` library and libraries that might light to encapsulate and re-export
//! the `apecs` library.
use quote::quote;
use syn::{
    punctuated::Punctuated, token::Comma, Data, DataStruct, DeriveInput, Field, Fields,
    FieldsNamed, FieldsUnnamed, Ident, Path, Type, WhereClause, WherePredicate,
};

fn collect_field_types(fields: &Punctuated<Field, Comma>) -> Vec<Type> {
    fields.iter().map(|x| x.ty.clone()).collect()
}

fn gen_identifiers(fields: &Punctuated<Field, Comma>) -> Vec<Ident> {
    fields.iter().map(|x| x.ident.clone().unwrap()).collect()
}

fn gen_from_body(path: &Path, ast: &Data, name: &Ident) -> (proc_macro2::TokenStream, Vec<Type>) {
    enum DataType {
        Struct,
        Tuple,
    }

    let (body, fields) = match *ast {
        Data::Struct(DataStruct {
            fields: Fields::Named(FieldsNamed { named: ref x, .. }),
            ..
        }) => (DataType::Struct, x),
        Data::Struct(DataStruct {
            fields: Fields::Unnamed(FieldsUnnamed { unnamed: ref x, .. }),
            ..
        }) => (DataType::Tuple, x),
        _ => panic!("Enums are not supported"),
    };

    let tys = collect_field_types(fields);

    let fetch_return = match body {
        DataType::Struct => {
            let identifiers = gen_identifiers(fields);

            quote! {
                #name {
                    #( #identifiers: #path::CanFetch::construct(loan_mngr)? ),*
                }
            }
        }
        DataType::Tuple => {
            let count = tys.len();
            let fetch = vec![quote! { #path::CanFetch::construct(loan_mngr)? }; count];

            quote! {
                #name ( #( #fetch ),* )
            }
        }
    };

    (fetch_return, tys)
}

/// Helper to create `CanFetch` derive macros with a configurable module prefix.
pub fn derive_canfetch(path: Path, input: DeriveInput) -> proc_macro2::TokenStream {
    let name = input.ident;
    let (construct_return, tys) = gen_from_body(&path, &input.data, &name);
    let mut generics = input.generics;
    {
        /// Adds a `CanFetch<'lt>` bound on each of the system data types.
        fn constrain_system_data_types(path: &Path, clause: &mut WhereClause, tys: &[Type]) {
            for ty in tys.iter() {
                let where_predicate: WherePredicate = syn::parse_quote!(#ty : #path::CanFetch);
                clause.predicates.push(where_predicate);
            }
        }

        let where_clause = generics.make_where_clause();
        constrain_system_data_types(&path, where_clause, &tys)
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics #path::CanFetch for #name #ty_generics #where_clause {
            fn borrows() -> Vec<#path::internal::Borrow> {
                let mut r = Vec::new();
                #({
                    r.extend(<#tys as #path::CanFetch>::borrows());
                })*
                r
            }

            fn construct(loan_mngr: &mut #path::internal::LoanManager) -> anyhow::Result<Self> {
                Ok(#construct_return)
            }

            fn plugin() -> #path::Plugin {
                #path::Plugin::default()
                    #(.with_plugin(<#tys as #path::CanFetch>::plugin()))*
            }
        }
    };

    output.into()
}

/// Helper to create `TryDefault` derive macros with a configurable module prefix.
pub fn derive_trydefault(path: Path, input: DeriveInput) -> proc_macro2::TokenStream {
    let name = input.ident;
    let mut generics = input.generics;
    {
        let where_clause = generics.make_where_clause();
        let where_predicate: WherePredicate = syn::parse_quote!(#name : Default);
        where_clause.predicates.push(where_predicate);
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let output = quote!{
        impl #impl_generics #path::TryDefault for #name #ty_generics #where_clause {
            fn try_default() -> Option<Self> {
                Some(Self::default())
            }
        }
    };
    output.into()
}
