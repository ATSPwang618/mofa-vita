//! Rust declarations -> native adapters and class metadata. No VM dependency.
// MSVC reports creation of the proc-macro DLL's import library as linker output.
#![cfg_attr(target_env = "msvc", allow(linker_messages))]
use std::collections::{BTreeMap, HashSet};
use zyn::{quote::quote, syn};

#[derive(zyn::Attribute)]
struct ClassOptions {
    name: String,
    #[zyn(default = "object".to_owned())]
    storage: String,
    #[zyn(default)]
    static_class: bool,
}
#[derive(zyn::Attribute)]
struct MemberOptions {
    #[zyn(default)]
    resumable: bool,
    #[zyn(default)]
    name: Option<String>,
    #[zyn(default)]
    class_only: bool,
    #[zyn(default)]
    hidden: bool,
}

#[zyn::attribute]
fn class(#[zyn(input)] item: syn::ItemMod, args: zyn::Args) -> zyn::TokenStream {
    let options = match ClassOptions::from_args(&args) {
        Ok(value) => value,
        Err(error) => return error.emit().into(),
    };
    match expand_class(item, options) {
        Ok(tokens) => tokens,
        Err(error) => error.into_compile_error(),
    }
}

#[derive(zyn::Attribute)]
struct FunctionOptions {
    #[zyn(default)]
    resumable: bool,
}
/// Keep the typed Rust function and generate `function_name::CALL` for exports.
/// Shares the class adapter's conversions, defaults and continuation contract.
#[zyn::attribute]
fn function(#[zyn(input)] item: syn::ItemFn, args: zyn::Args) -> zyn::TokenStream {
    let options = match FunctionOptions::from_args(&args) {
        Ok(value) => value,
        Err(error) => return error.emit().into(),
    };
    match expand_function(item, options) {
        Ok(tokens) => tokens,
        Err(error) => error.into_compile_error(),
    }
}
fn expand_function(
    mut item: syn::ItemFn,
    options: FunctionOptions,
) -> syn::Result<zyn::TokenStream> {
    let sig = &item.sig;
    if sig.asyncness.is_some()
        || sig.unsafety.is_some()
        || !sig.generics.params.is_empty()
        || sig.variadic.is_some()
        || sig.receiver().is_some()
    {
        return Err(syn::Error::new_spanned(
            sig,
            "native functions must be safe, synchronous, nongeneric free functions",
        ));
    }
    let mut parameters = Vec::new();
    for input in &mut item.sig.inputs {
        if let syn::FnArg::Typed(input) = input {
            parameters.push(parameter(input)?);
        }
    }
    let Arguments {
        conversions,
        arguments,
        ..
    } = arguments(&item.sig, &parameters)?;
    let name = &item.sig.ident;
    let visibility = &item.vis;
    let cfg: Vec<_> = item
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
        .collect();
    let question = fallible(&item.sig.output).then(|| quote!(?));
    let (kind, result, convert) = if options.resumable {
        (
            quote!(Resumable),
            quote!(::tjs_bind::NativeStep),
            quote!(Ok(value)),
        )
    } else {
        (
            quote!(Leaf),
            quote!(::tjs_bind::Value),
            quote!(::tjs_bind::IntoTjs::into_tjs(value, cx.heap_mut())),
        )
    };
    Ok(quote! {
        #item
        #(#cfg)*
        #visibility mod #name {
            use super::*;
            pub const CALL: ::tjs_bind::NativeCallable = ::tjs_bind::NativeCallable::#kind(invoke);
            fn invoke(cx: &mut ::tjs_bind::NativeCx<'_>, args: &[::tjs_bind::Value]) -> ::tjs_bind::NativeResult<#result> {
                let _ = (&cx, &args);
                #(#conversions)*
                let value = super::#name(#(#arguments),*) #question;
                #convert
            }
        }
    })
}

fn docs(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .filter_map(|attr| {
            if !attr.path().is_ident("doc") {
                return None;
            }
            let syn::Meta::NameValue(meta) = &attr.meta else {
                return None;
            };
            let syn::Expr::Lit(lit) = &meta.value else {
                return None;
            };
            let syn::Lit::Str(text) = &lit.lit else {
                return None;
            };
            Some(text.value())
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn type_name(ty: &syn::Type) -> Option<&syn::Ident> {
    match ty {
        syn::Type::Path(path) => path.path.segments.last().map(|s| &s.ident),
        syn::Type::Reference(reference) => type_name(&reference.elem),
        _ => None,
    }
}
fn fallible(output: &syn::ReturnType) -> bool {
    matches!(output, syn::ReturnType::Type(_, ty) if
        type_name(ty).is_some_and(|name| name == "Result" || name == "NativeResult"))
}
fn helper(attr: &syn::Attribute) -> bool {
    attr.path()
        .segments
        .first()
        .is_some_and(|segment| segment.ident == "tjs" || segment.ident == "tjs_bind")
}

#[derive(Default)]
struct Property {
    class_only: Option<bool>,
    hidden: Option<bool>,
    get: Option<zyn::TokenStream>,
    set: Option<zyn::TokenStream>,
    doc: String,
}

#[derive(Default)]
struct Parameter {
    coerce: bool,
    default: Option<syn::Expr>,
}
fn parameter(input: &mut syn::PatType) -> syn::Result<Parameter> {
    let mut result = Parameter::default();
    for attr in input.attrs.iter().filter(|a| helper(a)) {
        if type_name(&input.ty).is_some_and(|n| n == "NativeCx" || n == "RestArgs") {
            return Err(syn::Error::new_spanned(
                attr,
                "parameter options require a fixed script argument",
            ));
        }
        if !attr.path().is_ident("tjs") {
            return Err(syn::Error::new_spanned(
                attr,
                "use #[tjs(coerce, default = expression)] on parameters",
            ));
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("coerce") && !result.coerce {
                result.coerce = true;
            } else if meta.path.is_ident("default") && result.default.is_none() {
                result.default = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("unknown or duplicate parameter option"));
            }
            Ok(())
        })?;
    }
    input.attrs.retain(|a| !helper(a));
    Ok(result)
}

struct Arguments {
    conversions: Vec<zyn::TokenStream>,
    arguments: Vec<zyn::TokenStream>,
    count: usize,
    rest: bool,
}
fn arguments(sig: &syn::Signature, parameters: &[Parameter]) -> syn::Result<Arguments> {
    let mut conversions = Vec::new();
    let mut arguments = Vec::new();
    let mut count = 0_usize;
    let mut rest = false;
    let mut parameters = parameters.iter();
    for input in &sig.inputs {
        let syn::FnArg::Typed(input) = input else {
            continue;
        };
        let syn::Pat::Ident(_) = input.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &input.pat,
                "use a named native parameter",
            ));
        };
        // Adapter locals belong to the generated scope. Reusing a
        // user's parameter name can capture args/cx/state below.
        let name = zyn::format_ident!("__tjs_argument_{}", arguments.len());
        let parameter = parameters.next().expect("typed parameter options");
        let ty = &input.ty;
        if type_name(ty).is_some_and(|name| name == "NativeCx") {
            arguments.push(quote!(cx));
        } else if type_name(ty).is_some_and(|name| name == "RestArgs") {
            if rest {
                return Err(syn::Error::new_spanned(
                    input,
                    "only one RestArgs is allowed",
                ));
            }
            rest = true;
            conversions.push(quote!(let #name: #ty = &args[#count.min(args.len())..];));
            arguments.push(quote!(#name));
        } else {
            if rest {
                return Err(syn::Error::new_spanned(
                    input,
                    "RestArgs must follow fixed script arguments",
                ));
            }
            let conversion = if parameter.coerce {
                quote!(::tjs_bind::coerce_argument(args, #count, cx.heap())?)
            } else {
                quote!(::tjs_bind::argument(args, #count, cx.heap())?)
            };
            let conversion = if let Some(default) = &parameter.default {
                quote!(if args.len() > #count { #conversion } else { #default })
            } else {
                conversion
            };
            conversions.push(quote!(let #name: #ty = #conversion;));
            arguments.push(quote!(#name));
            count += 1;
        }
    }

    Ok(Arguments {
        conversions,
        arguments,
        count,
        rest,
    })
}

fn expand_class(mut item: syn::ItemMod, options: ClassOptions) -> syn::Result<zyn::TokenStream> {
    let class_doc = docs(&item.attrs);
    let (_, items) = item.content.as_mut().ok_or_else(|| {
        syn::Error::new(item.ident.span(), "native class requires an inline module")
    })?;
    let state = items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(state) if state.ident == "State" => Some(state),
            _ => None,
        })
        .ok_or_else(|| syn::Error::new(item.ident.span(), "native class requires struct State"))?;
    if !state.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &state.generics,
            "generic native State is not supported yet",
        ));
    }
    let mut wrappers = Vec::new();
    let mut methods = Vec::new();
    let mut constants = Vec::new();
    let mut names = HashSet::new();
    let mut properties: BTreeMap<String, Property> = BTreeMap::new();
    let mut constructor = None;
    let mut invalidate = None;
    let mut constructor_class_only = false;
    for item in items.iter_mut() {
        let syn::Item::Impl(implementation) = item else {
            continue;
        };
        if implementation.trait_.is_some()
            || !type_name(&implementation.self_ty).is_some_and(|name| name == "State")
        {
            continue;
        }
        for member in &mut implementation.items {
            if let syn::ImplItem::Const(constant) = member {
                for attr in constant.attrs.iter().filter(|a| helper(a)) {
                    if !attr
                        .path()
                        .segments
                        .last()
                        .is_some_and(|s| s.ident == "constant")
                    {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "use tjs::constant on exported constants",
                        ));
                    }
                    let args = match &attr.meta {
                        syn::Meta::Path(_) => zyn::Args::new(),
                        _ => attr.parse_args::<zyn::Args>()?,
                    };
                    let config = MemberOptions::from_args(&args)
                        .map_err(|e| syn::Error::new_spanned(attr, e.to_string()))?;
                    if config.resumable || config.class_only || config.hidden {
                        return Err(syn::Error::new_spanned(attr, "constants accept only name"));
                    }
                    if constant
                        .attrs
                        .iter()
                        .any(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
                    {
                        return Err(syn::Error::new_spanned(
                            constant,
                            "place cfg on the whole native class module for now",
                        ));
                    }
                    let name = config.name.unwrap_or_else(|| constant.ident.to_string());
                    if !names.insert(name.clone()) || properties.contains_key(&name) {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "duplicate native member name",
                        ));
                    }
                    let ident = &constant.ident;
                    constants.push(quote! {
                        let value = ::tjs_bind::IntoTjs::into_tjs(State::#ident, heap)?;
                        let key = heap.intern(&#name.encode_utf16().collect::<Vec<_>>());
                        heap.set_member(class, key, value)?;
                    });
                }
                constant.attrs.retain(|a| !helper(a));
                continue;
            }
            let syn::ImplItem::Fn(function) = member else {
                continue;
            };
            let exports: Vec<_> = function
                .attrs
                .iter()
                .filter(|a| helper(a))
                .cloned()
                .collect();
            function.attrs.retain(|a| !helper(a));
            let mut parameters = Vec::new();
            if !exports.is_empty() {
                for input in &mut function.sig.inputs {
                    if let syn::FnArg::Typed(input) = input {
                        parameters.push(parameter(input)?);
                    }
                }
            }
            for attr in exports {
                let kind = attr
                    .path()
                    .segments
                    .last()
                    .expect("attribute path")
                    .ident
                    .to_string();
                if !matches!(
                    kind.as_str(),
                    "constructor" | "method" | "getter" | "setter" | "invalidate"
                ) {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "unknown native member attribute",
                    ));
                }
                let args = match &attr.meta {
                    syn::Meta::Path(_) => zyn::Args::new(),
                    _ => attr.parse_args::<zyn::Args>()?,
                };
                let config = MemberOptions::from_args(&args)
                    .map_err(|e| syn::Error::new_spanned(&attr, e.to_string()))?;
                let class_only = config.class_only;
                let hidden = config.hidden;
                let resumable = config.resumable;
                if kind == "invalidate"
                    && (class_only || hidden || config.name.is_some() || options.static_class)
                {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "invalidate is an intrinsic instance hook, without name, hidden or class_only",
                    ));
                }
                if hidden && kind == "constructor" {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "hidden applies to methods and properties",
                    ));
                }
                let name = config
                    .name
                    .unwrap_or_else(|| function.sig.ident.to_string());
                let doc = docs(&function.attrs);
                let sig = &function.sig;
                if sig.asyncness.is_some()
                    || sig.unsafety.is_some()
                    || !sig.generics.params.is_empty()
                    || sig.variadic.is_some()
                {
                    return Err(syn::Error::new_spanned(
                        sig,
                        "native leaves must be safe, synchronous, nongeneric Rust methods",
                    ));
                }
                if function
                    .attrs
                    .iter()
                    .any(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
                {
                    return Err(syn::Error::new_spanned(
                        sig,
                        "place cfg on the whole native class module for now",
                    ));
                }
                let is_constructor = kind == "constructor";
                let receiver = sig.receiver();
                if is_constructor && receiver.is_some()
                    || receiver.is_some_and(|r| r.reference.is_none())
                {
                    return Err(syn::Error::new_spanned(
                        sig,
                        "constructors are associated functions; instance methods require &self or &mut self",
                    ));
                }
                let Arguments {
                    conversions,
                    arguments,
                    count,
                    rest,
                } = arguments(sig, &parameters)?;
                if (kind == "getter" && (count != 0 || rest))
                    || (kind == "setter" && (count != 1 || rest))
                {
                    return Err(syn::Error::new_spanned(
                        sig,
                        "getter takes no script arguments; setter takes one",
                    ));
                }
                if kind == "invalidate" && (count != 0 || rest) {
                    return Err(syn::Error::new_spanned(
                        sig,
                        "invalidate takes no script arguments",
                    ));
                }
                let wrapper = zyn::format_ident!("__tjs_member_{}", wrappers.len());
                let method = &sig.ident;
                let question = fallible(&sig.output).then(|| quote!(?));
                let body = if is_constructor && resumable {
                    if fallible(&sig.output) {
                        quote! { State::#method(#(#arguments),*) }
                    } else {
                        quote! { Ok(State::#method(#(#arguments),*)) }
                    }
                } else if is_constructor && options.static_class {
                    quote! {
                        let _: State = State::#method(#(#arguments),*) #question;
                        Ok(::tjs_bind::Value::Void)
                    }
                } else if is_constructor {
                    quote! {
                        let state: State = State::#method(#(#arguments),*) #question;
                        cx.construct(state)
                    }
                } else if receiver.is_none() {
                    let convert = if resumable {
                        quote!(Ok(value))
                    } else {
                        quote!(::tjs_bind::IntoTjs::into_tjs(value, cx.heap_mut()))
                    };
                    quote! {
                        let value = State::#method(#(#arguments),*) #question;
                        #convert
                    }
                } else {
                    let call = if fallible(&sig.output) {
                        quote!(state.#method(#(#arguments),*))
                    } else {
                        quote!(Ok(state.#method(#(#arguments),*)))
                    };
                    let convert = if resumable {
                        quote!(Ok(value))
                    } else {
                        quote!(::tjs_bind::IntoTjs::into_tjs(value, cx.heap_mut()))
                    };
                    quote! {
                        let value = cx.with_state::<State, _>(|state, cx| {
                            let _ = &cx;
                            #call
                        })?;
                        #convert
                    }
                };
                let result = if resumable {
                    quote!(::tjs_bind::NativeStep)
                } else {
                    quote!(::tjs_bind::Value)
                };
                wrappers.push(quote! {
                    fn #wrapper(cx: &mut ::tjs_bind::NativeCx<'_>, args: &[::tjs_bind::Value])
                        -> ::tjs_bind::NativeResult<#result> {
                        let _ = &args;
                        #(#conversions)*
                        #body
                    }
                });
                match kind.as_str() {
                    "invalidate" => {
                        let call = if resumable {
                            quote!(Resumable)
                        } else {
                            quote!(Leaf)
                        };
                        let callback = quote!(Some((::std::any::TypeId::of::<State>, ::tjs_bind::NativeCallable::#call(#wrapper))));
                        if invalidate.replace(callback).is_some() {
                            return Err(syn::Error::new_spanned(
                                attr,
                                "duplicate native invalidate hook",
                            ));
                        }
                    }
                    "constructor" => {
                        constructor_class_only = class_only;
                        let callable = if resumable {
                            quote!(::tjs_bind::NativeCallable::Resumable(#wrapper))
                        } else {
                            quote!(::tjs_bind::NativeCallable::Leaf(#wrapper))
                        };
                        if constructor.replace(callable).is_some() {
                            return Err(syn::Error::new_spanned(
                                attr,
                                "duplicate native constructor",
                            ));
                        }
                    }
                    "method" => {
                        if !names.insert(name.clone()) || properties.contains_key(&name) {
                            return Err(syn::Error::new_spanned(
                                attr,
                                "duplicate native member name",
                            ));
                        }
                        let callable = if resumable {
                            quote!(Resumable)
                        } else if name == "finalize"
                            && function.block.stmts.is_empty()
                            && arguments.is_empty()
                            && matches!(sig.output, syn::ReturnType::Default)
                        {
                            quote!(EmptyFinalizer)
                        } else {
                            quote!(Leaf)
                        };
                        methods.push(quote!(::tjs_bind::NativeMethod { hidden: #hidden, name: #name, doc: #doc, class_only: #class_only, call: ::tjs_bind::NativeCallable::#callable(#wrapper) }));
                    }
                    _ => {
                        if names.contains(&name) {
                            return Err(syn::Error::new_spanned(
                                attr,
                                "method/property name collision",
                            ));
                        }
                        let property = properties.entry(name).or_default();
                        if property
                            .class_only
                            .is_some_and(|previous| previous != class_only)
                        {
                            return Err(syn::Error::new_spanned(
                                attr,
                                "accessors must agree on class_only",
                            ));
                        }
                        if property.hidden.is_some_and(|previous| previous != hidden) {
                            return Err(syn::Error::new_spanned(
                                attr,
                                "accessors must agree on hidden",
                            ));
                        }
                        property.hidden = Some(hidden);
                        property.class_only = Some(class_only);
                        let slot = if kind == "getter" {
                            &mut property.get
                        } else {
                            &mut property.set
                        };
                        let callable = if resumable {
                            quote!(::tjs_bind::NativeCallable::Resumable(#wrapper))
                        } else {
                            quote!(::tjs_bind::NativeCallable::Leaf(#wrapper))
                        };
                        if slot.replace(callable).is_some() {
                            return Err(syn::Error::new_spanned(attr, "duplicate native accessor"));
                        }
                        if !doc.is_empty() {
                            property.doc = doc;
                        }
                    }
                }
            }
        }
    }
    let constructor = constructor
        .ok_or_else(|| syn::Error::new(item.ident.span(), "native class requires a constructor"))?;
    let invalidate = invalidate.unwrap_or_else(|| quote!(None));
    let mut property_tokens = Vec::new();
    for (name, property) in properties {
        let doc = property.doc;
        let class_only = property.class_only.unwrap_or(false);
        let hidden = property.hidden.unwrap_or(false);
        let getter = property
            .get
            .map_or_else(|| quote!(None), |get| quote!(Some(#get)));
        let setter = property
            .set
            .map_or_else(|| quote!(None), |set| quote!(Some(#set)));
        property_tokens.push(quote!(::tjs_bind::NativeProperty { hidden: #hidden, class_only: #class_only, name: #name, doc: #doc, get: #getter, set: #setter }));
    }
    let (storage, literal) = match options.storage.as_str() {
        "object" => (quote!(Object), quote!(None)),
        "array" | "dictionary" => {
            let storage = if options.storage == "array" {
                quote!(Array)
            } else {
                quote!(Dictionary)
            };
            (
                storage,
                quote!(Some(|heap, class| heap
                    .alloc_native(class, State::default())
                    .expect("registered intrinsic class"))),
            )
        }
        _ => {
            return Err(syn::Error::new(
                item.ident.span(),
                "storage must be object, array or dictionary",
            ));
        }
    };
    let initialize = if !options.static_class {
        quote!(|heap, object| heap.initialize_native_default::<State>(object))
    } else {
        quote!(|_, _| Err(::tjs_bind::NativeError::Message(
            "cannot create an instance of this class"
        )))
    };
    let name = options.name;
    let generated = zyn::zyn! {
        @for (wrapper in wrappers) { {{ wrapper }} }
        pub static CLASS: ::tjs_bind::NativeClass = ::tjs_bind::NativeClass {
            name: {{ name }},
            doc: {{ class_doc }},
            storage: ::tjs_bind::NativeStorage::{{ storage }},
            constructor: {{ constructor }},
            constructor_class_only: {{ constructor_class_only }},
            initialize: {{ initialize }},
            invalidate: {{ invalidate }},
            literal: {{ literal }},
            methods: &[ @for (method in methods) { {{ method }}, } ],
            properties: &[ @for (property in property_tokens) { {{ property }}, } ],
        };
        pub fn install(heap: &mut ::tjs_bind::Heap) -> ::tjs_bind::NativeResult<::tjs_bind::ObjId> {
            let class = heap.register_class(&CLASS)?;
            @for (constant in constants) { {{ constant }} }
            Ok(class)
        }
        /// Install class metadata and explicitly replace its per-heap class state.
        /// Ordinary installation preserves existing state and instances.
        pub fn install_with_state(heap: &mut ::tjs_bind::Heap, state: State) -> ::tjs_bind::NativeResult<::tjs_bind::ObjId> {
            let class = install(heap)?;
            heap.initialize_class_state::<State>(class)?;
            heap.with_native_state::<State, _>(class, |slot| *slot = state)?;
            Ok(class)
        }
        /// Access this binding's state on a managed object without VM reentry.
        pub fn with_state<R>(cx: &mut ::tjs_bind::NativeCx<'_>, owner: ::tjs_bind::ObjId, f: impl FnOnce(&mut State) -> R) -> ::tjs_bind::NativeResult<R> {
            cx.heap_mut().with_native_state::<State, R>(owner, f)
        }
    };
    let generated: syn::File = syn::parse2(generated.into())?;
    item.content
        .as_mut()
        .expect("inline module")
        .1
        .extend(generated.items);
    Ok(quote!(#item))
}

#[proc_macro_derive(Trace, attributes(trace))]
pub fn derive_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match trace_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}
struct TraceFields {
    members: Vec<syn::Member>,
    bindings: Vec<syn::Ident>,
    traced: Vec<syn::Ident>,
}
fn trace_fields(fields: &syn::Fields, generics: &mut syn::Generics) -> syn::Result<TraceFields> {
    let mut result = TraceFields {
        members: Vec::new(),
        bindings: Vec::new(),
        traced: Vec::new(),
    };
    for (index, field) in fields.iter().enumerate() {
        let mut skip = false;
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("trace")) {
            attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("skip") || skip {
                    return Err(meta.error("expected one skip = \"GC-free field rationale\""));
                }
                let reason: syn::LitStr = meta.value()?.parse()?;
                if reason.value().trim().is_empty() {
                    return Err(meta.error("skip requires a nonempty GC-free field rationale"));
                }
                skip = true;
                Ok(())
            })?;
        }
        let binding = zyn::format_ident!("__tjs_field_{index}");
        if !skip {
            // Concrete recursive fields need no where-clause: their calls are
            // checked in this implementation, avoiding self-referential bounds.
            if !generics.params.is_empty() {
                let ty = &field.ty;
                generics
                    .make_where_clause()
                    .predicates
                    .push(syn::parse_quote!(#ty: ::tjs_bind::Trace));
            }
            result.traced.push(binding.clone());
        }
        result.bindings.push(binding);
        result.members.push(
            field
                .ident
                .clone()
                .map(syn::Member::Named)
                .unwrap_or_else(|| syn::Member::Unnamed(index.into())),
        );
    }
    Ok(result)
}
fn trace_impl(input: syn::DeriveInput) -> syn::Result<zyn::TokenStream> {
    let name = input.ident;
    let mut generics = input.generics;
    let body = match input.data {
        syn::Data::Struct(data) => {
            let TraceFields {
                members,
                bindings,
                traced,
            } = trace_fields(&data.fields, &mut generics)?;
            quote! {
                let Self { #(#members: #bindings),* } = self;
                #( ::tjs_bind::Trace::trace(#traced, visit); )*
            }
        }
        syn::Data::Enum(data) => {
            let mut arms = Vec::new();
            for variant in data.variants {
                let variant_name = variant.ident;
                let TraceFields {
                    members,
                    bindings,
                    traced,
                } = trace_fields(&variant.fields, &mut generics)?;
                arms.push(quote! {
                    Self::#variant_name { #(#members: #bindings),* } => {
                        #( ::tjs_bind::Trace::trace(#traced, visit); )*
                    }
                });
            }
            quote!(match self { #(#arms),* })
        }
        syn::Data::Union(_) => {
            return Err(syn::Error::new(
                name.span(),
                "Trace does not support unions",
            ));
        }
    };
    let (implementation, arguments, clause) = generics.split_for_impl();
    Ok(quote! {
        impl #implementation ::tjs_bind::Trace for #name #arguments #clause {
            fn trace(&self, visit: &mut dyn FnMut(::tjs_bind::Value)) {
                #body
                let _ = visit;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_exports_report_declaration_errors() {
        for (member, expected) in [
            (
                "#[tjs::method] fn a(#[tjs(coerce, coerce)] x:i64) {}",
                "duplicate parameter option",
            ),
            (
                "#[tjs::method] fn a(#[tjs(default=1, default=2)] x:i64) {}",
                "duplicate parameter option",
            ),
            (
                "#[tjs::method] fn a(#[tjs(default=1)] cx: &mut NativeCx) {}",
                "fixed script argument",
            ),
            (
                "#[tjs::method] fn a(#[tjs(coerce)] args: RestArgs) {}",
                "fixed script argument",
            ),
            (
                "#[tjs::constant] const A:i64=1; #[tjs::method(name=\"A\")] fn a() {}",
                "duplicate native member",
            ),
            ("#[tjs::method] async fn run(&self) {}", "synchronous"),
            ("#[tjs::method] fn run(self) {}", "&self or &mut self"),
            (
                "#[tjs::getter] fn value(&self, arg: i64) {}",
                "getter takes no",
            ),
            ("#[tjs::setter] fn value(&mut self) {}", "setter takes one"),
            (
                "#[tjs::method] fn run(&self, rest: RestArgs, arg: i64) {}",
                "RestArgs must follow",
            ),
            ("#[tjs::unknown] fn run(&self) {}", "unknown native"),
            (
                "#[tjs::invalidate] fn cleanup(&self, arg: i64) {}",
                "invalidate takes no script arguments",
            ),
            (
                "#[tjs::invalidate(name = \"finalize\")] fn cleanup(&self) {}",
                "intrinsic instance hook",
            ),
            (
                "#[tjs::invalidate] fn a(&self) {} #[tjs::invalidate] fn b(&self) {}",
                "duplicate native invalidate hook",
            ),
            (
                "#[tjs::method(name = \"value\")] fn a(&self) {} #[tjs::getter(name = \"value\")] fn b(&self) {}",
                "collision",
            ),
            (
                "#[tjs::constructor] fn other() -> Self { Self }",
                "duplicate native constructor",
            ),
        ] {
            let input = syn::parse_str(&format!(
                "mod example {{ struct State; impl State {{ #[tjs::constructor] fn new() -> Self {{ Self }} {member} }} }}"
            )).unwrap();
            let error = expand_class(
                input,
                ClassOptions {
                    name: "Example".into(),
                    storage: "object".into(),
                    static_class: false,
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}: {member}");
        }
    }

    #[test]
    fn trace_skip_requires_an_auditable_reason() {
        for field in [
            "#[trace(skip)] field: Buffer",
            "#[trace(skip=\"\")] field: Buffer",
            "#[trace(ignore)] field: Buffer",
        ] {
            let input = syn::parse_str(&format!("struct State {{ {field} }}")).unwrap();
            assert!(trace_impl(input).is_err(), "{field}");
        }
    }
}
