use std::collections::HashMap;
use std::path::Path;

use vtkio::model::*;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, VtkDataArray, VtkMetadata};
use crate::types::{ColumnInfo, Dimension, FileMetadata};

/// Metadata extractor for VTK files (legacy and XML formats).
pub struct VtkExtractor;

impl Default for VtkExtractor {
    fn default() -> Self {
        Self
    }
}

impl VtkExtractor {
    pub fn new() -> Self {
        Self
    }
}

fn map_iobuffer_type(buf: &IOBuffer) -> DataType {
    match buf {
        IOBuffer::Bit(_) => DataType::Boolean,
        IOBuffer::U8(_) => DataType::UInt8,
        IOBuffer::I8(_) => DataType::Int8,
        IOBuffer::U16(_) => DataType::UInt16,
        IOBuffer::I16(_) => DataType::Int16,
        IOBuffer::U32(_) => DataType::UInt32,
        IOBuffer::I32(_) => DataType::Int32,
        IOBuffer::U64(_) => DataType::UInt64,
        IOBuffer::I64(_) => DataType::Int64,
        IOBuffer::F32(_) => DataType::Float32,
        IOBuffer::F64(_) => DataType::Float64,
    }
}

fn iobuffer_type_name(buf: &IOBuffer) -> &'static str {
    match buf {
        IOBuffer::Bit(_) => "bit",
        IOBuffer::U8(_) => "uint8",
        IOBuffer::I8(_) => "int8",
        IOBuffer::U16(_) => "uint16",
        IOBuffer::I16(_) => "int16",
        IOBuffer::U32(_) => "uint32",
        IOBuffer::I32(_) => "int32",
        IOBuffer::U64(_) => "uint64",
        IOBuffer::I64(_) => "int64",
        IOBuffer::F32(_) => "float32",
        IOBuffer::F64(_) => "float64",
    }
}

fn iobuffer_len(buf: &IOBuffer) -> usize {
    match buf {
        IOBuffer::Bit(v) => v.len(),
        IOBuffer::U8(v) => v.len(),
        IOBuffer::I8(v) => v.len(),
        IOBuffer::U16(v) => v.len(),
        IOBuffer::I16(v) => v.len(),
        IOBuffer::U32(v) => v.len(),
        IOBuffer::I32(v) => v.len(),
        IOBuffer::U64(v) => v.len(),
        IOBuffer::I64(v) => v.len(),
        IOBuffer::F32(v) => v.len(),
        IOBuffer::F64(v) => v.len(),
    }
}

fn num_components_for_elem(elem: &ElementType) -> u32 {
    match elem {
        ElementType::Scalars { num_comp, .. } => *num_comp,
        ElementType::Vectors => 3,
        ElementType::Normals => 3,
        ElementType::Tensors => 9,
        ElementType::TCoords(dim) => *dim,
        ElementType::ColorScalars(n) => *n,
        ElementType::LookupTable => 4,
        ElementType::Generic(n) => *n,
    }
}

fn collect_data_arrays(
    attrs: &[Attribute],
    prefix: &str,
    columns: &mut Vec<ColumnInfo>,
    vtk_arrays: &mut Vec<VtkDataArray>,
) {
    for attr in attrs {
        match attr {
            Attribute::DataArray(da) => {
                let num_comp = num_components_for_elem(&da.elem);
                let total_elements = iobuffer_len(&da.data);
                let num_tuples = if num_comp > 0 {
                    total_elements as u64 / num_comp as u64
                } else {
                    total_elements as u64
                };

                columns.push(ColumnInfo {
                    name: format!("{prefix}{}", da.name),
                    data_type: map_iobuffer_type(&da.data),
                    nullable: false,
                    metadata: HashMap::new(),
                    statistics: None,
                    classifications: vec![],
                });

                vtk_arrays.push(VtkDataArray {
                    name: da.name.clone(),
                    num_components: num_comp,
                    num_tuples,
                    data_type: iobuffer_type_name(&da.data).to_string(),
                });
            }
            Attribute::Field { data_array, .. } => {
                for fa in data_array {
                    let total_elements = iobuffer_len(&fa.data);
                    let num_tuples = if fa.elem > 0 {
                        total_elements as u64 / fa.elem as u64
                    } else {
                        total_elements as u64
                    };

                    columns.push(ColumnInfo {
                        name: format!("{prefix}{}", fa.name),
                        data_type: map_iobuffer_type(&fa.data),
                        nullable: false,
                        metadata: HashMap::new(),
                        statistics: None,
                        classifications: vec![],
                    });

                    vtk_arrays.push(VtkDataArray {
                        name: fa.name.clone(),
                        num_components: fa.elem,
                        num_tuples,
                        data_type: iobuffer_type_name(&fa.data).to_string(),
                    });
                }
            }
        }
    }
}

struct PieceInfo {
    dataset_type: String,
    num_points: Option<u64>,
    num_cells: Option<u64>,
    point_attrs: Vec<Attribute>,
    cell_attrs: Vec<Attribute>,
}

fn extract_piece_info(data: &DataSet) -> PieceInfo {
    match data {
        DataSet::UnstructuredGrid { pieces, .. } => {
            let mut total_points = 0u64;
            let mut total_cells = 0u64;
            let mut point_attrs = Vec::new();
            let mut cell_attrs = Vec::new();

            for piece in pieces {
                if let Piece::Inline(p) = piece {
                    total_points += p.num_points() as u64;
                    total_cells += p.cells.types.len() as u64;
                    point_attrs.extend(p.data.point.clone());
                    cell_attrs.extend(p.data.cell.clone());
                }
            }

            PieceInfo {
                dataset_type: "UnstructuredGrid".into(),
                num_points: Some(total_points),
                num_cells: Some(total_cells),
                point_attrs,
                cell_attrs,
            }
        }
        DataSet::PolyData { pieces, .. } => {
            let mut total_points = 0u64;
            let mut total_cells = 0u64;
            let mut point_attrs = Vec::new();
            let mut cell_attrs = Vec::new();

            for piece in pieces {
                if let Piece::Inline(p) = piece {
                    total_points += p.num_points() as u64;
                    let cells: u64 = [&p.verts, &p.lines, &p.polys, &p.strips]
                        .iter()
                        .filter_map(|v| v.as_ref())
                        .map(|v| v.num_cells() as u64)
                        .sum();
                    total_cells += cells;
                    point_attrs.extend(p.data.point.clone());
                    cell_attrs.extend(p.data.cell.clone());
                }
            }

            PieceInfo {
                dataset_type: "PolyData".into(),
                num_points: Some(total_points),
                num_cells: Some(total_cells),
                point_attrs,
                cell_attrs,
            }
        }
        DataSet::StructuredGrid { pieces, .. } => {
            let mut total_points = 0u64;
            let mut point_attrs = Vec::new();
            let mut cell_attrs = Vec::new();

            for piece in pieces {
                if let Piece::Inline(p) = piece {
                    total_points += (iobuffer_len(&p.points) / 3) as u64;
                    point_attrs.extend(p.data.point.clone());
                    cell_attrs.extend(p.data.cell.clone());
                }
            }

            PieceInfo {
                dataset_type: "StructuredGrid".into(),
                num_points: Some(total_points),
                num_cells: None,
                point_attrs,
                cell_attrs,
            }
        }
        DataSet::RectilinearGrid { pieces, .. } => {
            let mut point_attrs = Vec::new();
            let mut cell_attrs = Vec::new();

            for piece in pieces {
                if let Piece::Inline(p) = piece {
                    point_attrs.extend(p.data.point.clone());
                    cell_attrs.extend(p.data.cell.clone());
                }
            }

            PieceInfo {
                dataset_type: "RectilinearGrid".into(),
                num_points: None,
                num_cells: None,
                point_attrs,
                cell_attrs,
            }
        }
        DataSet::ImageData { pieces, .. } => {
            let mut point_attrs = Vec::new();
            let mut cell_attrs = Vec::new();

            for piece in pieces {
                if let Piece::Inline(p) = piece {
                    point_attrs.extend(p.data.point.clone());
                    cell_attrs.extend(p.data.cell.clone());
                }
            }

            PieceInfo {
                dataset_type: "ImageData".into(),
                num_points: None,
                num_cells: None,
                point_attrs,
                cell_attrs,
            }
        }
        DataSet::Field { .. } => PieceInfo {
            dataset_type: "Field".into(),
            num_points: None,
            num_cells: None,
            point_attrs: Vec::new(),
            cell_attrs: Vec::new(),
        },
    }
}

fn vtk_error(path: &Path, e: vtkio::Error) -> MetadataError {
    MetadataError::ParseError {
        format: "vtk".into(),
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

impl MetadataExtractor for VtkExtractor {
    fn format(&self) -> FileFormat {
        FileFormat::VtkLegacy
    }

    fn extensions(&self) -> &[&str] {
        &["vtk", "vtu", "vtp", "vts", "vtr", "vti"]
    }

    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let vtk = Vtk::import(path).map_err(|e| vtk_error(path, e))?;

        let version_str = format!("{}.{}", vtk.version.major, vtk.version.minor);
        let title = if vtk.title.is_empty() {
            None
        } else {
            Some(vtk.title.clone())
        };

        let format = crate::detect::detect_from_extension(path).unwrap_or(FileFormat::VtkLegacy);

        let info = extract_piece_info(&vtk.data);

        let mut columns = Vec::new();
        let mut point_data_arrays = Vec::new();
        let mut cell_data_arrays = Vec::new();

        collect_data_arrays(
            &info.point_attrs,
            "point:",
            &mut columns,
            &mut point_data_arrays,
        );
        collect_data_arrays(
            &info.cell_attrs,
            "cell:",
            &mut columns,
            &mut cell_data_arrays,
        );

        let mut dimensions = Vec::new();
        if let Some(np) = info.num_points {
            dimensions.push(Dimension {
                name: "points".into(),
                size: Some(np),
            });
        }
        if let Some(nc) = info.num_cells {
            dimensions.push(Dimension {
                name: "cells".into(),
                size: Some(nc),
            });
        }

        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

        Ok(FileMetadata {
            format,
            mime_type: None,
            num_rows: info.num_points,
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
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Vtk(VtkMetadata {
                version: Some(version_str),
                title,
                dataset_type: info.dataset_type,
                num_points: info.num_points,
                num_cells: info.num_cells,
                point_data: point_data_arrays,
                cell_data: cell_data_arrays,
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
