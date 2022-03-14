use quote::quote;
use syn::{
    parse_macro_input, punctuated::Punctuated, token::Comma, Data, DataStruct, DeriveInput,
    Field, Fields, FieldsNamed, FieldsUnnamed, Ident, Lifetime, Type,
    WhereClause, WherePredicate,
};

fn collect_field_types(fields: &Punctuated<Field, Comma>) -> Vec<Type> {
    fields.iter().map(|x| x.ty.clone()).collect()
}

fn gen_identifiers(fields: &Punctuated<Field, Comma>) -> Vec<Ident> {
    fields.iter().map(|x| x.ident.clone().unwrap()).collect()
}

/// Adds a `CanFetch<'lt>` bound on each of the system data types.
fn constrain_system_data_types(clause: &mut WhereClause, fetch_lt: &Lifetime, tys: &[Type]) {
    for ty in tys.iter() {
        let where_predicate: WherePredicate = syn::parse_quote!(#ty : CanFetch< #fetch_lt >);
        clause.predicates.push(where_predicate);
    }
}

fn gen_from_body(ast: &Data, name: &Ident) -> (proc_macro2::TokenStream, Vec<Type>) {
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
                    #( #identifiers: apecs::CanFetch::construct(tx, fields)? ),*
                }
            }
        }
        DataType::Tuple => {
            let count = tys.len();
            let fetch = vec![quote! { SystemData::fetch(world) }; count];

            quote! {
                #name ( #( #fetch ),* )
            }
        }
    };

    (fetch_return, tys)
}

#[proc_macro_derive(CanFetch)]
pub fn derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = parse_macro_input!(input);

    let name = input.ident;
    let (construct_return, tys) = gen_from_body(&input.data, &name);
    let lt = input
        .generics
        .lifetimes()
        .next()
        .expect("There has to be at least one lifetime");
    let mut generics = input.generics.clone();
    {
        let where_clause = generics.make_where_clause();
        constrain_system_data_types(where_clause, &lt.lifetime, &tys)
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics apecs::CanFetch<#lt> for #name #ty_generics #where_clause {
            fn reads() -> Vec<apecs::ResourceId> {
                let mut r = Vec::new();
                #({
                    r.extend(<#tys as apecs::CanFetch>::reads());
                })*
                r
            }

            fn writes() -> Vec<apecs::ResourceId> {
                let mut r = Vec::new();
                #({
                    r.extend(<#tys as apecs::CanFetch>::writes());
                })*
                r
            }

            fn construct(
                tx: &'a apecs::spsc::Sender<std::collections::HashMap<apecs::ResourceId, apecs::Resource>>,
                fields: &mut std::collections::HashMap<apecs::ResourceId, apecs::Resource>,
            ) -> anyhow::Result<Self> {
                Ok(#construct_return)
            }
        }
    };

    output.into()
}
