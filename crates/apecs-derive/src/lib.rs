use std::collections::VecDeque;

use proc_macro2::Span;
use quote::quote;
use syn::{
    parse_macro_input, punctuated::Punctuated, token::Comma, Data, DataStruct, DeriveInput, Field,
    Fields, FieldsNamed, FieldsUnnamed, Ident, Type, TypeTuple, WhereClause, WherePredicate,
};

fn collect_field_types(fields: &Punctuated<Field, Comma>) -> Vec<Type> {
    fields.iter().map(|x| x.ty.clone()).collect()
}

fn gen_identifiers(fields: &Punctuated<Field, Comma>) -> Vec<Ident> {
    fields.iter().map(|x| x.ident.clone().unwrap()).collect()
}

/// Adds a `CanFetch<'lt>` bound on each of the system data types.
fn constrain_system_data_types(clause: &mut WhereClause, tys: &[Type]) {
    for ty in tys.iter() {
        let where_predicate: WherePredicate = syn::parse_quote!(#ty : CanFetch);
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
                    #( #identifiers: apecs::CanFetch::construct(tx.clone(), fields)? ),*
                }
            }
        }
        DataType::Tuple => {
            let count = tys.len();
            let fetch = vec![quote! { apecs::CanFetch::construct(tx.clone(), fields) }; count];

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
    let mut generics = input.generics.clone();
    {
        let where_clause = generics.make_where_clause();
        constrain_system_data_types(where_clause, &tys)
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics apecs::CanFetch for #name #ty_generics #where_clause {
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
                tx: apecs::mpsc::Sender<(apecs::ResourceId, apecs::Resource)>,
                fields: &mut std::collections::HashMap<apecs::ResourceId, apecs::Resource>,
            ) -> anyhow::Result<Self> {
                Ok(#construct_return)
            }
        }
    };

    output.into()
}

#[proc_macro]
pub fn impl_canfetch_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let output = quote! {
        impl <#(#tys:CanFetch),*> CanFetch for #tuple {
            fn reads() -> Vec<ResourceId> {
                let mut r = Vec::new();
                #(r.extend(<#tys as CanFetch>::reads());)*
                r
            }

            fn writes() -> Vec<ResourceId> {
                let mut w = Vec::new();
                #(w.extend(<#tys as CanFetch>::writes());)*
                w
            }

            fn construct(
                tx: mpsc::Sender<(ResourceId, Resource)>,
                fields: &mut std::collections::HashMap<ResourceId, Resource>,
            ) -> anyhow::Result<Self> {
                Ok((
                    #(#tys::construct(tx.clone(), fields)?),*
                ))
            }
        }
    };

    output.into()
}

#[proc_macro]
pub fn impl_join_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let wheres = tys.iter().map(|ty| quote! {
        #ty: Iterator,
        <#ty as Iterator>::Item: StorageComponent,
    }).collect::<Vec<_>>();
    let next_names = (0..tys.len()).map(|n| {
        let name = Ident::new(&format!("next_{}", n), Span::call_site());
        let n = syn::Member::Unnamed(n.into());
        (name, n)
    }).collect::<VecDeque<_>>();
    let nexts = next_names.iter().map(|(name, n)| quote! {
        let mut #name = self.0.#n.next()?.split();
    }).collect::<VecDeque<_>>();
    let while_check = {
        let mut nexts = next_names.clone();
        let first = nexts.pop_front().unwrap();
        let next_a = first.0;
        let checks = nexts
            .iter()
            .map(|(name, _)| quote!{ #next_a.0 == #name.0 })
            .fold(None, |acc, check| match acc {
                Some(prev) => Some(quote!{ #prev && #check }),
                None => Some(quote!{ #check }),
            })
            .unwrap();
        let syncs = nexts
            .iter()
            .map(|(name, n)| quote!{
                sync(&mut self.0.0, &mut #next_a, &mut self.0.#n, &mut #name)?;
            })
            .collect::<Vec<_>>();
        let result = nexts
            .iter()
            .fold(
                quote!{#next_a.0, #next_a.1},
                |acc, (name, _)| quote!{#acc, #name.1}
            );
        quote!{
            while!(#checks) {
                #(#syncs)*
            }
            Some((#result))
        }
    };

    let self_ns = next_names.iter().map(|(_, n)| quote!{ self.#n.join() }).collect::<Vec<_>>();
    let nexts = nexts.into_iter().collect::<Vec<_>>();

    let iterator_for_joined_iter = quote! {
        impl <#(#tys),*> Iterator for JoinedIter<#tuple>
        where
            #(#tys: Iterator,)*
            #(<#tys as Iterator>::Item: StorageComponent,)*
        {
            type Item = (
                u32,
                #(<<#tys as Iterator>::Item as StorageComponent>::Component),*
            );

            fn next(&mut self) -> Option<Self::Item> {
                #(#nexts)*
                #while_check
            }
        }
    };

    let join_for_tuple = quote!{
        impl <#(#tys),*> Join for #tuple
        where
            #(#tys: Join,)*
            #(#tys::Iter: Iterator,)*
            #(<#tys::Iter as Iterator>::Item: StorageComponent,)*
        {
            type Iter = JoinedIter<( #(#tys::Iter),* )>;

            fn join(self) -> Self::Iter {
                JoinedIter(( #(#self_ns),* ))
            }
        }
    };

    let output = quote! {
        #iterator_for_joined_iter

        #join_for_tuple
    };

    output.into()
}
