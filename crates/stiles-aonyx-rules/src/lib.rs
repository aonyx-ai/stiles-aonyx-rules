//! The release unit of the Aonyx rules for stiles
//!
//! Otto releases this workspace as one lockstep unit. Such a unit takes the
//! name of its repository, and it must hold a crate with that name. This
//! crate gives the unit its name, and it holds no code.
//!
//! Every crate in the workspace inherits the version of the workspace, so
//! one release moves the facade and every rule together. A project loads
//! the rules from a commit of this repository, so no crate is published.
//!
//! When the rules go to crates.io, this crate becomes the entry point that
//! a project names. It then depends on each rule, and one release publishes
//! all of them.
