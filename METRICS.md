    Blocking waiting for file lock on package cache
    Blocking waiting for file lock on package cache
    Blocking waiting for file lock on package cache
 Downloading crates ...
error: failed to download `cpufeatures v0.3.1`

Caused by:
  unable to get packages from source

Caused by:
  failed to parse manifest at `/usr/local/cargo/registry/src/index.crates.io-6f17d22bba15001f/cpufeatures-0.3.1/Cargo.toml`

Caused by:
  feature `edition2024` is required

  The package requires the Cargo feature called `edition2024`, but that feature is not stabilized in this version of Cargo (1.83.0 (5ffbef321 2024-10-29)).
  Consider trying a newer version of Cargo (this may require the nightly release).
  See https://doc.rust-lang.org/nightly/cargo/reference/unstable.html#edition-2024 for more information about the status of this feature.
