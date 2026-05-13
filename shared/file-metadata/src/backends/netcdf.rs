//! HDF5 and NetCDF metadata extraction via the `netcdf` crate.
//!
//! The `netcdf` crate (georust) links to system `libnetcdf` which itself
//! links `libhdf5`, allowing both formats to be read through a single
//! dependency.

use std::collections::HashMap;
use std::path::Path;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, Hdf5Group, Hdf5Metadata, NetCdfMetadata};
use crate::types::{AttributeValue, ColumnInfo, Dimension, FileMetadata};

// ============================================================================
// Shared helpers
// ============================================================================

fn map_nc_type(nc_type: &netcdf::types::NcVariableType) -> DataType {
    use netcdf::types::{FloatType, IntType, NcVariableType};
    match nc_type {
        NcVariableType::Int(IntType::I8) => DataType::Int8,
        NcVariableType::Int(IntType::I16) => DataType::Int16,
        NcVariableType::Int(IntType::I32) => DataType::Int32,
        NcVariableType::Int(IntType::I64) => DataType::Int64,
        NcVariableType::Int(IntType::U8) => DataType::UInt8,
        NcVariableType::Int(IntType::U16) => DataType::UInt16,
        NcVariableType::Int(IntType::U32) => DataType::UInt32,
        NcVariableType::Int(IntType::U64) => DataType::UInt64,
        NcVariableType::Float(FloatType::F32) => DataType::Float32,
        NcVariableType::Float(FloatType::F64) => DataType::Float64,
        NcVariableType::String | NcVariableType::Char => DataType::String,
        other => DataType::Unknown(format!("{other:?}")),
    }
}

fn map_nc_attribute(nc_attr: &netcdf::AttributeValue) -> AttributeValue {
    use netcdf::AttributeValue as NcAttr;
    match nc_attr {
        // Scalar integers
        NcAttr::Schar(v) => AttributeValue::Int(i64::from(*v)),
        NcAttr::Uchar(v) => AttributeValue::Int(i64::from(*v)),
        NcAttr::Short(v) => AttributeValue::Int(i64::from(*v)),
        NcAttr::Ushort(v) => AttributeValue::Int(i64::from(*v)),
        NcAttr::Int(v) => AttributeValue::Int(i64::from(*v)),
        NcAttr::Uint(v) => AttributeValue::Int(i64::from(*v)),
        NcAttr::Longlong(v) => AttributeValue::Int(*v),
        NcAttr::Ulonglong(v) => AttributeValue::Int(*v as i64),
        // Scalar floats
        NcAttr::Float(v) => AttributeValue::Float(f64::from(*v)),
        NcAttr::Double(v) => AttributeValue::Float(*v),
        // Strings
        NcAttr::Str(s) => AttributeValue::String(s.clone()),
        // Vector types — serialize to compact representation
        NcAttr::Uchars(v) => AttributeValue::Bytes(v.clone()),
        NcAttr::Schars(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Shorts(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Ushorts(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Ints(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Uints(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Longlongs(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Ulonglongs(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Floats(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Doubles(v) => AttributeValue::String(format!("{v:?}")),
        NcAttr::Strs(v) => AttributeValue::String(v.join(", ")),
    }
}

fn collect_attributes<'a>(
    attrs: impl Iterator<Item = netcdf::Attribute<'a>>,
) -> HashMap<String, AttributeValue> {
    let mut map = HashMap::new();
    for attr in attrs {
        if let Ok(val) = attr.value() {
            map.insert(attr.name().to_string(), map_nc_attribute(&val));
        }
    }
    map
}

fn collect_group_attributes<'a>(
    attrs: impl Iterator<Item = netcdf::Attribute<'a>>,
) -> Vec<(String, AttributeValue)> {
    let mut result = Vec::new();
    for attr in attrs {
        if let Ok(val) = attr.value() {
            result.push((attr.name().to_string(), map_nc_attribute(&val)));
        }
    }
    result
}

fn open_nc_error(path: &Path, e: netcdf::Error) -> MetadataError {
    MetadataError::ParseError {
        format: "netcdf".into(),
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

// ============================================================================
// NetCdfExtractor
// ============================================================================

/// Metadata extractor for NetCDF files (.nc, .nc4, .netcdf).
///
/// Extracts variable names, types, dimensions, global attributes, and
/// NetCDF-specific metadata (conventions, unlimited dimensions).
pub struct NetCdfExtractor;

impl Default for NetCdfExtractor {
    fn default() -> Self {
        Self
    }
}

impl NetCdfExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl MetadataExtractor for NetCdfExtractor {
    fn format(&self) -> FileFormat {
        FileFormat::NetCdf
    }

    fn extensions(&self) -> &[&str] {
        &["nc", "nc4", "netcdf"]
    }

    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let file = netcdf::open(path).map_err(|e| open_nc_error(path, e))?;

        // Dimensions
        let mut unlimited_dims = Vec::new();
        let dimensions: Vec<Dimension> = file
            .dimensions()
            .map(|d| {
                if d.is_unlimited() {
                    unlimited_dims.push(d.name());
                }
                Dimension {
                    name: d.name(),
                    size: Some(d.len() as u64),
                }
            })
            .collect();

        // Variables -> columns
        let mut columns = Vec::new();
        let mut variable_names = Vec::new();

        for var in file.variables() {
            variable_names.push(var.name());
            columns.push(ColumnInfo {
                name: var.name(),
                data_type: map_nc_type(&var.vartype()),
                nullable: true,
                metadata: HashMap::new(),
                statistics: None,
                classifications: vec![],
            });
        }

        // num_rows: element count of the first variable
        let num_rows = file.variables().next().map(|v| v.len() as u64);

        // Root-level attributes
        let attributes = collect_attributes(file.attributes());

        // Conventions global attribute
        let conventions = file
            .attribute("Conventions")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Str(s) => Some(s),
                _ => None,
            });

        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

        Ok(FileMetadata {
            format: FileFormat::NetCdf,
            mime_type: None,
            num_rows,
            num_columns: Some(columns.len() as u64),
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names,
            dimensions,
            columns,
            attributes,
            format_specific: Some(FormatMetadata::NetCdf(NetCdfMetadata {
                conventions,
                unlimited_dimensions: unlimited_dims,
                variables: variable_names,
            })),
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }
}

// ============================================================================
// Hdf5Extractor
// ============================================================================

/// Metadata extractor for HDF5 files (.h5, .hdf5, .he5).
///
/// Opens HDF5 files through `libnetcdf`'s NC4 driver and extracts
/// hierarchical group structure, dataset types, and attributes.
pub struct Hdf5Extractor;

impl Default for Hdf5Extractor {
    fn default() -> Self {
        Self
    }
}

impl Hdf5Extractor {
    pub fn new() -> Self {
        Self
    }
}

impl MetadataExtractor for Hdf5Extractor {
    fn format(&self) -> FileFormat {
        FileFormat::Hdf5
    }

    fn extensions(&self) -> &[&str] {
        &["h5", "hdf5", "he5"]
    }

    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let file = netcdf::open(path).map_err(|e| open_nc_error(path, e))?;

        let mut groups = Vec::new();
        let mut all_columns = Vec::new();

        // Root-level variables (datasets)
        let root_datasets: Vec<String> = file.variables().map(|v| v.name()).collect();
        let root_attrs = collect_group_attributes(file.attributes());

        for var in file.variables() {
            all_columns.push(ColumnInfo {
                name: var.name(),
                data_type: map_nc_type(&var.vartype()),
                nullable: true,
                metadata: HashMap::new(),
                statistics: None,
                classifications: vec![],
            });
        }

        groups.push(Hdf5Group {
            path: "/".into(),
            datasets: root_datasets,
            attributes: root_attrs,
        });

        // Walk sub-groups (NC4/HDF5 only — classic NetCDF has no groups)
        if let Ok(subgroups) = file.groups() {
            for group in subgroups {
                walk_hdf5_group(&group, "/", &mut groups, &mut all_columns);
            }
        }

        // Dimensions from root level
        let dimensions: Vec<Dimension> = file
            .dimensions()
            .map(|d| Dimension {
                name: d.name(),
                size: Some(d.len() as u64),
            })
            .collect();

        let num_rows = file.variables().next().map(|v| v.len() as u64);
        let root_attributes = collect_attributes(file.attributes());
        let column_names: Vec<String> = all_columns.iter().map(|c| c.name.clone()).collect();

        Ok(FileMetadata {
            format: FileFormat::Hdf5,
            mime_type: None,
            num_rows,
            num_columns: Some(all_columns.len() as u64),
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names,
            dimensions,
            columns: all_columns,
            attributes: root_attributes,
            format_specific: Some(FormatMetadata::Hdf5(Hdf5Metadata { groups })),
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }
}

/// Recursively walk HDF5 groups, collecting group metadata and datasets.
fn walk_hdf5_group(
    group: &netcdf::Group<'_>,
    parent_path: &str,
    groups: &mut Vec<Hdf5Group>,
    columns: &mut Vec<ColumnInfo>,
) {
    let group_name = group.name();
    let group_path = if parent_path == "/" {
        format!("/{group_name}")
    } else {
        format!("{parent_path}/{group_name}")
    };

    let datasets: Vec<String> = group.variables().map(|v| v.name()).collect();
    let attrs = collect_group_attributes(group.attributes());

    for var in group.variables() {
        let full_name = format!("{group_path}/{}", var.name());
        columns.push(ColumnInfo {
            name: full_name,
            data_type: map_nc_type(&var.vartype()),
            nullable: true,
            metadata: HashMap::new(),
            statistics: None,
            classifications: vec![],
        });
    }

    groups.push(Hdf5Group {
        path: group_path.clone(),
        datasets,
        attributes: attrs,
    });

    // Recurse into sub-groups
    for subgroup in group.groups() {
        walk_hdf5_group(&subgroup, &group_path, groups, columns);
    }
}
