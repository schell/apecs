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
                    #( #identifiers: apecs::CanFetch::construct(loan_mngr)? ),*
                }
            }
        }
        DataType::Tuple => {
            let count = tys.len();
            let fetch = vec![quote! { apecs::CanFetch::construct(loan_mngr) }; count];

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
            fn borrows() -> Vec<apecs::schedule::Borrow> {
                let mut r = Vec::new();
                #({
                    r.extend(<#tys as apecs::CanFetch>::borrows());
                })*
                r
            }

            fn construct(loan_mngr: &mut apecs::resource_manager::LoanManager) -> apecs::anyhow::Result<Self> {
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

#[proc_macro_derive(StoredComponent_Vec)]
pub fn derive_stored_component_vec(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = parse_macro_input!(input);

    let name = input.ident;
    let generics = input.generics;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics apecs::storage::separate::StoredComponent for #name #ty_generics {
            type StorageType = apecs::storage::separate::VecStorage<Self>;
        }
    };

    output.into()
}

#[proc_macro_derive(StoredComponent_Range)]
pub fn derive_stored_component_range(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = parse_macro_input!(input);

    let name = input.ident;
    let generics = input.generics;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics apecs::storage::separate::StoredComponent for #name #ty_generics {
            type StorageType = apecs::storage::separate::RangeStore<Self>;
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
            fn borrows() -> Vec<apecs::schedule::Borrow> {
                let mut bs = Vec::new();
                #(bs.extend(<#tys as apecs::CanFetch>::borrows());)*
                bs
            }

            fn construct(loan_mngr: &mut apecs::resource_manager::LoanManager) -> apecs::anyhow::Result<Self> {
                Ok((
                    #(#tys::construct(loan_mngr)?),*
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
        let result = nexts
            .iter()
            .fold(quote! {#next_a}, |acc, (name, _)| quote! {#acc, #name});
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
            #(#tys::Item: HasId,)*
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
            #(<#tys::Iter as Iterator>::Item: HasId,)*
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
                #ty: rayon::iter::IntoParallelIterator<Iter = #iter>
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
        impl <#(#tys,)* #(#iters,)* #(#comps,)*> apecs::storage::separated::join::ParJoin for #tuple
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

#[proc_macro]
pub fn impl_isquery_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let nvars: Vec<Ident> = (0..tys.len())
        .map(|n| Ident::new(&format!("n{}", n), Span::call_site()) )
        .collect();
    let mvars: Vec<Ident> = (0..tys.len())
        .map(|n| Ident::new(&format!("m{}", n), Span::call_site()) )
        .collect();
    let extend_impl = tys.iter().zip(nvars.iter().zip(mvars.iter())).skip(1).map(|(ty, (n, m))| quote! {
        #ty::extend_locked_columns(#n, #m, None);
    });
    let query_result_zip = tys
        .iter()
        .fold(None, |prev, ty| {
            Some(
                prev.map_or_else(
                    || quote! {#ty::QueryResult<'a>},
                    |p| {
                        quote! {std::iter::Zip<#p, #ty::QueryResult<'a>>}
                    },
                )
            )
        })
        .unwrap();
    let query_result_fn_param = tys.iter().fold(None, |prev, ty| {
        Some(
            prev.map_or_else(
                || quote! {#ty::QueryRow<'a>},
                |p| quote! {(#p, #ty::QueryRow<'a>)}
            )
        )
    })
    .unwrap();
    let par_query_result_zip = tys
        .iter()
        .fold(None, |prev, ty| {
            Some(
                prev.map_or_else(
                    || quote! {#ty::ParQueryResult<'a>},
                    |p| {
                        quote! {rayon::iter::Zip<#p, #ty::ParQueryResult<'a>>}
                    },
                )
            )
        })
        .unwrap();
    let iter_mut_impl_zip = tys.iter().zip(nvars.iter()).fold(None, |prev, (ty, n)| {
        Some(
            prev.map_or_else(
                || quote! {#ty::iter_mut(#n)},
                |p| quote! {#p.zip(#ty::iter_mut(#n))}
            )
        )
    }).unwrap();
    let iter_one_impl_zip = tys.iter().zip(nvars.iter()).fold(None, |prev, (ty, n)| {
        Some(
            prev.map_or_else(
                || quote! {#ty::iter_one(#n, index)},
                |p| quote! {#p.zip(#ty::iter_one(#n, index))}
            )
        )
    }).unwrap();
    let par_iter_mut_impl_zip = tys.iter().zip(nvars.iter()).fold(None, |prev, (ty, n)| {
        Some(
            prev.map_or_else(
                || quote! {#ty::par_iter_mut(#n)},
                |p| quote! {#p.zip(#ty::par_iter_mut(#n))}
            )
        )
    }).unwrap();
    let nvar_tuple_list = nvars.iter().fold(None, |prev, n| Some(
        prev.map_or_else(|| quote!{#n}, |p| quote! {(#p, #n)})
    ))
    .unwrap();
    let iter_mut_impl = quote! {
        #iter_mut_impl_zip.map(|#nvar_tuple_list| (#(#nvars),*))
    };
    let iter_one_impl = quote! {
        #iter_one_impl_zip.map(|#nvar_tuple_list| (#(#nvars),*))
    };
    let par_iter_mut_impl = quote! {
        #par_iter_mut_impl_zip.map(|#nvar_tuple_list| (#(#nvars),*))
    };


    let isquery_for_tuple = quote! {
        impl <#(#tys),*> apecs::storage::archetype::IsQuery for #tuple
        where
            #(#tys: IsQuery),*
        {
            type LockedColumns<'a> = (
                #(#tys::LockedColumns<'a>),*
            );

            type ExtensionColumns = (
                #(#tys::ExtensionColumns),*
            );

            type QueryResult<'a> = std::iter::Map<
                #query_result_zip,
                fn(
                    #query_result_fn_param,
                ) -> (#(#tys::QueryRow<'a>),*),
            >;

            type ParQueryResult<'a> = rayon::iter::Map<
                #par_query_result_zip,
                fn(
                    #query_result_fn_param,
                ) -> (#(#tys::QueryRow<'a>),*),
            >;

            type QueryRow<'a> = (#(#tys::QueryRow<'a>),*);

            #[inline]
            fn borrows() -> Vec<Borrow> {
                let mut bs = vec![];
                #(bs.extend(#tys::borrows());)*
                bs
            }

            #[inline]
            fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
                (#(#tys::lock_columns(arch)),*)
            }

            fn extend_locked_columns<'a, 'b>(
                (#(#nvars),*): &'b mut Self::LockedColumns<'a>,
                (#(#mvars),*): Self::ExtensionColumns,
                output_ids: Option<(&mut Vec<usize>, &mut usize)>,
            ) {
                A::extend_locked_columns(n0, m0, output_ids);
                #(#extend_impl)*
            }

            #[inline]
            fn iter_mut<'a, 'b>(
                (#(#nvars),*): &'b mut Self::LockedColumns<'a>
            ) -> Self::QueryResult<'b> {
                #iter_mut_impl
            }

            #[inline]
            fn iter_one<'a, 'b>(
                (#(#nvars),*): &'b mut Self::LockedColumns<'a>,
                index: usize,
            ) -> Self::QueryResult<'b> {
                #iter_one_impl
            }

            #[inline]
            fn par_iter_mut<'a, 'b>(
                (#(#nvars),*): &'b mut Self::LockedColumns<'a>
            ) -> Self::ParQueryResult<'b> {
                #par_iter_mut_impl
            }
        }
    };

    isquery_for_tuple.into()
}

#[proc_macro]
pub fn impl_isbundle_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let ns = (0..tys.len()).map(|n| {
        syn::Member::Unnamed(n.into())
    }).collect::<Vec<_>>();
    let isbundle_for_tuple = quote! {
        impl <#(#tys),*> apecs::storage::archetype::IsBundle for #tuple
        where
            #(#tys: Send + Sync + 'static),*
        {
            type EntryBundle = (
                #(Entry<#tys>),*
            );
            type MutBundle = (
                #(&'static mut #tys),*
            );

            fn unordered_types() -> SmallVec<[TypeId; 4]> {
                smallvec![
                    #(TypeId::of::<#tys>()),*
                ]
            }

            fn empty_vecs() -> SmallVec<[AnyVec<dyn Send + Sync + 'static>; 4]> {
                smallvec![
                    #(AnyVec::new::<#tys>()),*
                ]
            }

            fn try_from_any_bundle(mut bundle: AnyBundle) -> anyhow::Result<Self> {
                Ok((
                    #(bundle.remove::<#tys>(&TypeId::of::<#tys>())?),*
                ))
            }

            fn into_vecs(self) -> SmallVec<[AnyVec<dyn Send + Sync + 'static>; 4]> {
                smallvec![
                    #(AnyVec::wrap(self.#ns)),*
                ]
            }

            fn into_entry_bundle(self, entity_id: usize) -> Self::EntryBundle {
                (
                    #(Entry::new(entity_id, self.#ns)),*
                )
            }

            fn from_entry_bundle(entry_bundle: Self::EntryBundle) -> Self {
                (
                    #(entry_bundle.#ns.into_inner()),*
                )
            }

        }
    };

    isbundle_for_tuple.into()
}
