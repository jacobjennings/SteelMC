//! Asynchronous filesystem calls used by server storage.
//!
//! Browser builds have no Tokio filesystem driver. They use the standard
//! library calls, which report `Unsupported` there. World generation never
//! reaches these paths.

#[cfg(not(target_family = "wasm"))]
pub use tokio::fs::{create_dir_all, read_to_string, remove_dir_all, write};

#[cfg(target_family = "wasm")]
pub use browser::{create_dir_all, read_to_string, remove_dir_all, write};

#[cfg(target_family = "wasm")]
mod browser {
    use std::{fs, io, path::Path};

    /// Reads a whole file as UTF-8 text.
    ///
    /// # Errors
    /// Returns the platform error. Browsers report `Unsupported`.
    pub async fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
        fs::read_to_string(path)
    }

    /// Writes a whole file.
    ///
    /// # Errors
    /// Returns the platform error. Browsers report `Unsupported`.
    pub async fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        fs::write(path, contents)
    }

    /// Creates a directory and its parents.
    ///
    /// # Errors
    /// Returns the platform error. Browsers report `Unsupported`.
    pub async fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    /// Removes a directory tree.
    ///
    /// # Errors
    /// Returns the platform error. Browsers report `Unsupported`.
    pub async fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        fs::remove_dir_all(path)
    }
}
