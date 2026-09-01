//! The path the visitor actually asked for.
//!
//! Two things rewrite a request URI before a handler sees it. Language
//! negotiation strips a `/{lang}/` prefix, and alias resolution replaces an
//! alias with its source. By the time an item handler runs, `/it/why` has become
//! `/item/{uuid}` and both facts about the address are gone.
//!
//! `current_path` in the render context is that rewritten path, which is what a
//! route needs and the wrong thing to compare a menu link against: a menu that
//! links to `/why` can never match `/item/{uuid}`, so the active trail is dead on
//! every aliased page. `RequestedPath` keeps the original so a template can
//! compare like with like.

/// The request path as received, before any rewriting.
///
/// Set once, at the top of language negotiation, which is the first middleware
/// to touch the URI. Read from request extensions by a route and put into the
/// render context as `requested_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedPath(pub String);
