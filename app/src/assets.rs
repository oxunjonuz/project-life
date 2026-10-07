//! The window's files, compiled into the binary.
//!
//! Embedding them keeps the app to one moving part: whatever is on screen is what was built, and it
//! cannot be swapped under the user by an edit in a resource folder.

pub const INDEX_HTML: &str = include_str!("../ui/index.html");
pub const APP_CSS: &str = include_str!("../ui/app.css");
pub const APP_JS: &str = include_str!("../ui/app.js");
