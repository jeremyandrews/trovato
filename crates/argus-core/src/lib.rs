//! Host-agnostic pipeline core for the Argus news-intelligence plugin.
//!
//! Argus ingests RSS/Atom feeds, scores each article's relevance against a
//! topic with a cheap model (the *decide* stage), and advances survivors
//! through analyze/embed/cluster/summarize stages. This crate holds all of that
//! logic as pure functions and trait-bounded orchestration, with no dependency
//! on the kernel host, wasm, or a database.
//!
//! # Shape
//!
//! - [`model`] — pure domain types ([`model::PipelineState`], [`model::Stage`],
//!   [`model::ParsedArticle`], [`model::Decision`]).
//! - [`ports`] — the injected boundaries ([`ports::LlmProvider`],
//!   [`ports::Fetcher`], [`ports::Store`], [`ports::JobQueue`]).
//! - [`provider`] — a scripted [`provider::MockProvider`] and the lenient-JSON
//!   recovery the decide stage uses on sloppy model output.
//! - [`feed`] — RSS and Atom parsing (M1-5).
//! - [`dedup`] — content hashing for near-duplicate detection (M1-6).
//! - [`decide`] — relevance scoring against a topic threshold (M1-7).
//! - [`schedule`] — interval-based round-robin due-feed selection (M1-9).
//! - [`analyze`] — deep analysis and entity extraction from one call (M2).
//! - [`entity`] — entity normalization and fuzzy alias resolution (M2).
//! - [`embed`] — lexical feature vectors and cosine similarity (M2).
//! - [`cluster`] — story membership decisions and their scoring (M2).
//! - [`summarize`] — multi-source story synthesis and its rate limit (M2).
//! - [`budget`] — daily spend accounting and the pause gate (M2).
//! - [`config`] — the Item-backed feed/topic configuration contract and the
//!   coercion an admin edit passes through (M3).
//! - [`reader`] — per-user reactions, read state and their toggle rules (M3).
//! - [`notify`] — notification channels, payload rendering and dispatch (M4).
//! - [`ratelimit`] — debounce, quiet hours, digest collapse, retry backoff (M4).
//! - [`judge`] — whether a re-summarized story materially changed (M4).
//! - [`pipeline`] — stage orchestration wired over the ports.
//!
//! Everything here is depended on by the `plugins/argus` cdylib, which supplies
//! the port implementations over kernel host functions. Keeping the core pure
//! is the hedge in ARCHITECTURE.md §9.6: the same core wraps in a native
//! harness with no rewrite if the pure-plugin shape ever needs it.

#![forbid(unsafe_code)]

pub mod analyze;
pub mod budget;
pub mod cluster;
pub mod config;
pub mod decide;
pub mod dedup;
pub mod embed;
pub mod entity;
pub mod error;
pub mod feed;
pub mod judge;
pub mod model;
pub mod notify;
pub mod pipeline;
pub mod ports;
pub mod provider;
pub mod ratelimit;
pub mod reader;
pub mod schedule;
pub mod summarize;

pub use error::{CoreError, CoreResult};
