//! Defines the service provider interface for storage back-end implementors.

use super::error::Error;
use async_trait::async_trait;
use chrono::prelude::{DateTime, Utc};
use itertools::Itertools;
use std::{
    fmt::{self, Debug, Formatter, Write},
    path::Path,
    result,
    time::SystemTime,
};

/// Tells if STOR/RETR restarts are supported by the storage back-end
/// i.e. starting from a different byte offset.
pub const FEATURE_RESTART: u32 = 0b0000_0001;

/// Result type used by traits in this module
pub type Result<T> = result::Result<T, Error>;

/// Represents the metadata of a _FTP File_
pub trait Metadata {
    /// Returns the length (size) of the file in bytes.
    fn len(&self) -> u64;

    /// Returns `self.len() == 0`.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns true if the path is a directory.
    fn is_dir(&self) -> bool;

    /// Returns true if the path is a file.
    fn is_file(&self) -> bool;

    /// Returns true if the path is a symbolic link.
    fn is_symlink(&self) -> bool;

    /// Returns the last modified time of the path.
    fn modified(&self) -> Result<SystemTime>;

    /// Returns the `gid` of the file.
    fn gid(&self) -> u32;

    /// Returns the `uid` of the file.
    fn uid(&self) -> u32;

    /// Returns the `permissions` of the file. The default implementation assumes unix permissions
    /// and defaults to "rwxr-xr-x" (octal 7755)
    fn permissions(&self) -> Permissions {
        Permissions(0o7755)
    }
}

/// Represents the permissions of a _FTP File_
pub struct Permissions(u32);

const PERM_READ: u32 = 0b100100100;
const PERM_WRITE: u32 = 0b010010010;
const PERM_EXEC: u32 = 0b001001001;
const PERM_USER: u32 = 0b111000000;
const PERM_GROUP: u32 = 0b000111000;
const PERM_OTHERS: u32 = 0b000000111;

impl std::fmt::Display for Permissions {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_char(if self.0 & PERM_USER & PERM_READ > 0 { 'r' } else { '-' })?;
        f.write_char(if self.0 & PERM_USER & PERM_WRITE > 0 { 'w' } else { '-' })?;
        f.write_char(if self.0 & PERM_USER & PERM_EXEC > 0 { 'x' } else { '-' })?;
        f.write_char(if self.0 & PERM_GROUP & PERM_READ > 0 { 'r' } else { '-' })?;
        f.write_char(if self.0 & PERM_GROUP & PERM_WRITE > 0 { 'w' } else { '-' })?;
        f.write_char(if self.0 & PERM_GROUP & PERM_EXEC > 0 { 'x' } else { '-' })?;
        f.write_char(if self.0 & PERM_OTHERS & PERM_READ > 0 { 'r' } else { '-' })?;
        f.write_char(if self.0 & PERM_OTHERS & PERM_WRITE > 0 { 'w' } else { '-' })?;
        f.write_char(if self.0 & PERM_OTHERS & PERM_EXEC > 0 { 'x' } else { '-' })?;
        Ok(())
    }
}

/// Fileinfo contains the path and `Metadata` of a file.
///
/// [`Metadata`]: ./trait.Metadata.html
#[derive(Clone)]
pub struct Fileinfo<P, M>
where
    P: AsRef<Path>,
    M: Metadata,
{
    /// The full path to the file
    pub path: P,
    /// The file's metadata
    pub metadata: M,
}

impl<P, M> std::fmt::Display for Fileinfo<P, M>
where
    P: AsRef<Path>,
    M: Metadata,
{
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let modified: String = self
            .metadata
            .modified()
            .map(|x| DateTime::<Utc>::from(x).format("%b %d %H:%M").to_string())
            .unwrap_or_else(|_| "--- -- --:--".to_string());
        let basename = self.path.as_ref().components().last();
        let path = match basename {
            Some(v) => v.as_os_str().to_string_lossy(),
            None => {
                return Err(std::fmt::Error);
            }
        };
        let perms = format!("{}", self.metadata.permissions());
        #[allow(clippy::write_literal)]
        write!(
            f,
            "{filetype}{permissions} {owner:>12} {group:>12} {size:#14} {modified:>12} {path}",
            filetype = if self.metadata.is_dir() {
                "d"
            } else if self.metadata.is_symlink() {
                "l"
            } else {
                "-"
            },
            permissions = perms,
            owner = self.metadata.uid(),
            group = self.metadata.gid(),
            size = self.metadata.len(),
            modified = modified,
            path = path,
        )
    }
}

/// The `StorageBackend` trait defines a common interface to different storage backends for our FTP
/// [`Server`], e.g. for a [`Filesystem`] or Google Cloud Storage.
///
/// [`Server`]: ../struct.Server.html
/// [`filesystem`]: filesystem/struct.Filesystem.html
#[async_trait]
pub trait StorageBackend<U: Sync + Send + Debug>: Send + Sync + Debug {
    /// The concrete type of the _metadata_ used by this storage backend.
    type Metadata: Metadata + Sync + Send;

    /// Tells which optional features are supported by the storage back-end
    /// Return a value with bits set according to the FEATURE_* constants.
    fn supported_features(&self) -> u32 {
        0
    }

    /// Returns the `Metadata` for the given file.
    ///
    /// [`Metadata`]: ./trait.Metadata.html
    async fn metadata<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, path: P) -> Result<Self::Metadata>;

    /// Returns the list of files in the given directory.
    async fn list<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, path: P) -> Result<Vec<Fileinfo<std::path::PathBuf, Self::Metadata>>>
    where
        <Self as StorageBackend<U>>::Metadata: Metadata;

    /// Returns some bytes that make up a directory listing that can immediately be sent to the client.
    #[allow(clippy::type_complexity)]
    #[tracing_attributes::instrument]
    async fn list_fmt<P>(&self, user: &Option<U>, path: P) -> std::result::Result<std::io::Cursor<Vec<u8>>, Error>
    where
        P: AsRef<Path> + Send + Debug,
        Self::Metadata: Metadata + 'static,
    {
        let list = self.list(user, path).await?;

        let file_infos: Vec<u8> = list.iter().map(|fi| format!("{}\r\n", fi).into_bytes()).concat();

        Ok(std::io::Cursor::new(file_infos))
    }

    /// Returns some bytes that make up a NLST directory listing (only the basename) that can
    /// immediately be sent to the client.
    #[allow(clippy::type_complexity)]
    #[tracing_attributes::instrument]
    async fn nlst<P>(&self, user: &Option<U>, path: P) -> std::result::Result<std::io::Cursor<Vec<u8>>, std::io::Error>
    where
        P: AsRef<Path> + Send + Debug,
        Self::Metadata: Metadata + 'static,
    {
        let list = self.list(user, path).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::Other))?;

        let bytes = list
            .iter()
            .map(|file| {
                let info = file.path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("")).to_str().unwrap_or("");
                format!("{}\r\n", info).into_bytes()
            })
            .concat();
        Ok(std::io::Cursor::new(bytes))
    }

    /// Gets the content of the given FTP file from offset start_pos file by copying it to the output writer.
    /// The starting position will only be greater than zero if the storage back-end implementation
    /// advertises to support partial reads through the supported_features method i.e. the result
    /// from supported_features yield 1 if a logical and operation is applied with FEATURE_RESTART.
    async fn get_into<'a, P, W: ?Sized>(&self, user: &Option<U>, path: P, start_pos: u64, output: &'a mut W) -> Result<u64>
    where
        W: tokio::io::AsyncWrite + Unpin + Sync + Send,
        P: AsRef<Path> + Send + Debug,
    {
        let mut reader = self.get(user, path, start_pos).await?;
        Ok(tokio::io::copy(&mut reader, output).await?)
    }

    /// Returns the content of the given file from offset start_pos.
    /// The starting position will only be greater than zero if the storage back-end implementation
    /// advertises to support partial reads through the supported_features method i.e. the result
    /// from supported_features yield 1 if a logical and operation is applied with FEATURE_RESTART.
    async fn get<P: AsRef<Path> + Send + Debug>(
        &self,
        user: &Option<U>,
        path: P,
        start_pos: u64,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>>;

    /// Writes bytes from the given reader to the specified path starting at offset start_pos in the file
    async fn put<'a, P, R: ?Sized>(&self, user: &Option<U>, input: &'a mut R, path: P, start_pos: u64) -> Result<u64>
    where
        R: tokio::io::AsyncRead + Unpin + Sync + Send,
        P: AsRef<Path> + Send + Debug;

    /// Deletes the file at the given path.
    async fn del<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, path: P) -> Result<()>;

    /// Creates the given directory.
    async fn mkd<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, path: P) -> Result<()>;

    /// Renames the given file to the given new filename.
    async fn rename<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, from: P, to: P) -> Result<()>;

    /// Deletes the given directory.
    async fn rmd<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, path: P) -> Result<()>;

    /// Changes the working directory to the given path.
    async fn cwd<P: AsRef<Path> + Send + Debug>(&self, user: &Option<U>, path: P) -> Result<()>;
}
