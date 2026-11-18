//! Language analysis for OpenEdge ABL: lexer, preprocessor, statement parser,
//! schema (.df) support, lint rules and the formatter. Shared by the language
//! server and the `abl` CLI.

pub mod analysis;
pub mod builtins;
pub mod deps;
pub mod df;
pub mod features;
pub mod format;
pub mod handles;
pub mod keywords;
pub mod langref;
pub mod lexer;
pub mod line_index;
pub mod lint;
pub mod messages;
pub mod oo;
pub mod schema;
pub mod unittest;
pub mod usage;
pub mod workspace;
