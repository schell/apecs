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
    let mut generics = input.generics;
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
                fields: &mut apecs::FxHashMap<apecs::ResourceId, apecs::FetchReadyResource>,
            ) -> apecs::anyhow::Result<Self> {
                Ok(#construct_return)
            }

            fn plugin() -> apecs::plugins::Plugin {
                apecs::plugins::Plugin::default()
                    #(.with_plugin(<#tys as apecs::CanFetch>::plugin()))*
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
        impl <#(#tys:apecs::CanFetch),*> apecs::CanFetch for #tuple {
            fn reads() -> Vec<apecs::ResourceId> {
                let mut r = Vec::new();
                #(r.extend(<#tys as apecs::CanFetch>::reads());)*
                r
            }

            fn writes() -> Vec<apecs::ResourceId> {
                let mut w = Vec::new();
                #(w.extend(<#tys as apecs::CanFetch>::writes());)*
                w
            }

            fn construct(
                tx: apecs::mpsc::Sender<(apecs::ResourceId, apecs::Resource)>,
                fields: &mut apecs::FxHashMap<apecs::ResourceId, apecs::FetchReadyResource>,
            ) -> apecs::anyhow::Result<Self> {
                Ok((
                    #(#tys::construct(tx.clone(), fields)?),*
                ))
            }

            fn plugin() -> apecs::plugins::Plugin {
                apecs::plugins::Plugin::default()
                    #(.with_plugin(<#tys as apecs::CanFetch>::plugin()))*
            }
        }
    };

    output.into()
}

#[proc_macro]
pub fn impl_join_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let next_names = (0..tys.len())
        .map(|n| {
            let name = Ident::new(&format!("next_{}", n), Span::call_site());
            let n = syn::Member::Unnamed(n.into());
            (name, n)
        })
        .collect::<VecDeque<_>>();
    let nexts = next_names.iter().map(|(name, n)| {
        quote! {
            let mut #name = self.0.#n.next()?;
        }
    });
    let while_check = {
        let mut nexts = next_names.clone();
        let first = nexts.pop_front().unwrap();
        let next_a = first.0;
        let checks = nexts
            .iter()
            .map(|(name, _)| quote! { #next_a.id() == #name.id() })
            .fold(None, |acc, check| match acc {
                Some(prev) => Some(quote! { #prev && #check }),
                None => Some(quote! { #check }),
            })
            .unwrap();
        let syncs = nexts
            .iter()
            .map(|(name, n)| {
                quote! {
                    sync(&mut self.0.0, &mut #next_a, &mut self.0.#n, &mut #name)?;
                }
            })
            .collect::<Vec<_>>();
        let result = nexts.iter().fold(
            quote! {#next_a},
            |acc, (name, _)| quote! {#acc, #name},
        );
        quote! {
            while!(#checks) {
                #(#syncs)*
            }
            Some((#result))
        }
    };

    let self_ns = next_names
        .iter()
        .map(|(_, n)| quote! { self.#n.join() })
        .collect::<Vec<_>>();
    let nexts = nexts.into_iter().collect::<Vec<_>>();

    let iterator_for_joined_iter = quote! {
        impl <#(#tys),*> Iterator for JoinedIter<#tuple>
        where
            #(#tys: Iterator,)*
            #(#tys::Item: IsEntry,)*
        {
            type Item = (
                #(#tys::Item),*
            );

            fn next(&mut self) -> Option<Self::Item> {
                #(#nexts)*
                #while_check
            }
        }
    };

    let join_for_tuple = quote! {
        impl <#(#tys),* > Join for #tuple
        where
            #(#tys: Join,)*
            #(<#tys::Iter as Iterator>::Item: IsEntry,)*
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

#[proc_macro]
pub fn impl_parjoin_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    #[allow(clippy::type_complexity)]
    let (iters_comps, maybes_vals): (Vec<(Ident, Ident)>, Vec<(Ident, Ident)>) = (0..tys.len())
        .map(|n| {
            let iter = Ident::new(&format!("Iter{}", n), Span::call_site());
            let comp = Ident::new(&format!("Comp{}", n), Span::call_site());
            let maybe = Ident::new(&format!("m{}", n), Span::call_site());
            let val = Ident::new(&format!("n{}", n), Span::call_site());
            ((iter, comp), (maybe, val))
        })
        .unzip();
    let (iters, comps): (Vec<_>, Vec<_>) = iters_comps.into_iter().unzip();
    let (maybes, vals): (Vec<_>, Vec<_>) = maybes_vals.into_iter().unzip();
    let store_constraints = tys
        .iter()
        .zip(iters.iter())
        .map(|(ty, iter)| {
            quote! {
                #ty: IntoParallelIterator<Iter = #iter>
            }
        })
        .collect::<Vec<_>>();
    let iter_constraints: Vec<_> = iters
        .iter()
        .zip(comps.iter())
        .map(|(iter, comp)| {
            quote! {
                #iter: rayon::iter::IndexedParallelIterator<Item = Option<#comp>>
            }
        })
        .collect();
    let unwraps: Vec<_> = vals
        .iter()
        .zip(maybes.iter())
        .map(|(val, may)| {
            quote! {
                let #val = #may?;
            }
        })
        .collect();

    let join_for_tuple = quote! {
        impl <#(#tys,)* #(#iters,)* #(#comps,)*> apecs::join::ParJoin for #tuple
        where
            #(#store_constraints,)*
            #(#iter_constraints,)*
            #(#comps: Send,)*
        {
            type Iter = rayon::iter::FilterMap<
                    rayon::iter::MultiZip<(#(#iters),*)>,
                fn((#(Option<#comps>),*)) -> Option<(#(#comps),*)>,
            >;

            fn par_join(self) -> Self::Iter {
                self.into_par_iter().filter_map(|(#(#maybes),*)| {
                    #(#unwraps)*
                    Some((#(#vals),*))
                })
            }
        }
    };

    join_for_tuple.into()
}
