//! End-to-end tests: two mock Jellyfin upstreams (one over TLS with a private
//! CA), the real proxy in between, and a client doing what a TV app does.

mod mock;
mod setup;
mod tests;
