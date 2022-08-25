use proc_macro2::Span;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Ident, TypeTuple};

#[proc_macro_derive(CanFetch)]
pub fn derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = parse_macro_input!(input);
    let path = quote::format_ident!("apecs");
    apecs_derive_canfetch::derive_canfetch(path, input).into()
}

#[proc_macro]
pub fn impl_canfetch_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let output = quote! {
        impl <#(#tys:apecs::CanFetch),*> apecs::CanFetch for #tuple {
            fn borrows() -> Vec<apecs::internal::Borrow> {
                let mut bs = Vec::new();
                #(bs.extend(<#tys as apecs::CanFetch>::borrows());)*
                bs
            }

            fn construct(loan_mngr: &mut apecs::internal::LoanManager) -> apecs::anyhow::Result<Self> {
                Ok((
                    #(#tys::construct(loan_mngr)?),*
                ))
            }

            fn plugin() -> apecs::Plugin {
                apecs::Plugin::default()
                    #(.with_plugin(<#tys as apecs::CanFetch>::plugin()))*
            }
        }
    };

    output.into()
}

#[proc_macro]
pub fn impl_isquery_tuple(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let tuple: TypeTuple = parse_macro_input!(input);
    let tys = tuple.elems.iter().collect::<Vec<_>>();
    let nvars: Vec<Ident> = (0..tys.len())
        .map(|n| Ident::new(&format!("n{}", n), Span::call_site()))
        .collect();
    let mvars: Vec<Ident> = (0..tys.len())
        .map(|n| Ident::new(&format!("m{}", n), Span::call_site()))
        .collect();
    let extend_impl = tys
        .iter()
        .zip(nvars.iter().zip(mvars.iter()))
        .skip(1)
        .map(|(ty, (n, m))| {
            quote! {
                #ty::extend_locked_columns(#n, #m, None);
            }
        });
    let query_result_zip = tys
        .iter()
        .fold(None, |prev, ty| {
            Some(prev.map_or_else(
                || quote! {#ty::QueryResult<'a>},
                |p| {
                    quote! {std::iter::Zip<#p, #ty::QueryResult<'a>>}
                },
            ))
        })
        .unwrap();
    let query_result_fn_param = tys
        .iter()
        .fold(None, |prev, ty| {
            Some(prev.map_or_else(
                || quote! {#ty::QueryRow<'a>},
                |p| quote! {(#p, #ty::QueryRow<'a>)},
            ))
        })
        .unwrap();
    let par_query_result_zip = tys
        .iter()
        .fold(None, |prev, ty| {
            Some(prev.map_or_else(
                || quote! {#ty::ParQueryResult<'a>},
                |p| {
                    quote! {rayon::iter::Zip<#p, #ty::ParQueryResult<'a>>}
                },
            ))
        })
        .unwrap();
    let iter_mut_impl_zip = tys
        .iter()
        .zip(nvars.iter())
        .fold(None, |prev, (ty, n)| {
            Some(prev.map_or_else(
                || quote! {#ty::iter_mut(#n)},
                |p| quote! {#p.zip(#ty::iter_mut(#n))},
            ))
        })
        .unwrap();
    let iter_one_impl_zip = tys
        .iter()
        .zip(nvars.iter())
        .fold(None, |prev, (ty, n)| {
            Some(prev.map_or_else(
                || quote! {#ty::iter_one(#n, index)},
                |p| quote! {#p.zip(#ty::iter_one(#n, index))},
            ))
        })
        .unwrap();
    let par_iter_mut_impl_zip = tys
        .iter()
        .zip(nvars.iter())
        .fold(None, |prev, (ty, n)| {
            Some(prev.map_or_else(
                || quote! {#ty::par_iter_mut(len, #n)},
                |p| quote! {#p.zip(#ty::par_iter_mut(len, #n))},
            ))
        })
        .unwrap();
    let nvar_tuple_list = nvars
        .iter()
        .fold(None, |prev, n| {
            Some(prev.map_or_else(|| quote! {#n}, |p| quote! {(#p, #n)}))
        })
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
                len: usize,
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
    let ns = (0..tys.len())
        .map(|n| syn::Member::Unnamed(n.into()))
        .collect::<Vec<_>>();
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
