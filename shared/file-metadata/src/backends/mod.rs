#[cfg(feature = "csv")]
pub mod csv;

#[cfg(feature = "json")]
pub mod json;

#[cfg(feature = "parquet")]
pub mod parquet;

#[cfg(feature = "image")]
pub mod image;

#[cfg(any(feature = "audio", feature = "video"))]
pub(crate) mod media_common;

#[cfg(feature = "audio")]
pub mod audio;

#[cfg(feature = "video")]
pub mod video;

#[cfg(feature = "zip")]
pub mod zip;

#[cfg(feature = "excel")]
pub mod excel;

#[cfg(feature = "arrow")]
pub mod arrow;

#[cfg(feature = "netcdf")]
pub mod netcdf;

#[cfg(feature = "zarr")]
pub mod zarr;

#[cfg(feature = "vtk")]
pub mod vtk;

#[cfg(feature = "toml")]
pub mod toml;

#[cfg(feature = "yaml")]
pub mod yaml;

#[cfg(feature = "markdown")]
pub mod markdown;

#[cfg(feature = "xml")]
pub mod xml;

#[cfg(feature = "html")]
pub mod html;

#[cfg(feature = "ini")]
pub mod ini;

#[cfg(feature = "env")]
pub mod env;

#[cfg(feature = "txt")]
pub mod txt;
