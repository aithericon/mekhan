#![cfg(feature = "vtk")]

use fmeta::extractor::MetadataExtractor;
use fmeta::format::{FileFormat, FormatMetadata, VtkMetadata};
use fmeta::{detect_format, VtkExtractor};
use vtkio::model::*;

fn create_unstructured_grid_vtk(path: &std::path::Path) {
    let vtk = Vtk {
        version: Version::new((4, 2)),
        byte_order: ByteOrder::BigEndian,
        title: String::from("test mesh"),
        file_path: None,
        data: DataSet::inline(UnstructuredGridPiece {
            points: vec![
                0.0f64, 0.0, 0.0, // point 0
                1.0, 0.0, 0.0, // point 1
                0.0, 1.0, 0.0, // point 2
            ]
            .into(),
            cells: Cells {
                cell_verts: VertexNumbers::Legacy {
                    num_cells: 1,
                    vertices: vec![3, 0, 1, 2],
                },
                types: vec![CellType::Triangle],
            },
            data: Attributes {
                point: vec![Attribute::DataArray(DataArrayBase {
                    name: String::from("pressure"),
                    elem: ElementType::Scalars {
                        num_comp: 1,
                        lookup_table: None,
                    },
                    data: vec![1.0f64, 2.0, 3.0].into(),
                })],
                cell: vec![Attribute::DataArray(DataArrayBase {
                    name: String::from("temperature"),
                    elem: ElementType::Scalars {
                        num_comp: 1,
                        lookup_table: None,
                    },
                    data: vec![300.0f64].into(),
                })],
            },
        }),
    };
    vtk.export_ascii(path).unwrap();
}

fn create_polydata_vtk(path: &std::path::Path) {
    let vtk = Vtk {
        version: Version::new((4, 2)),
        byte_order: ByteOrder::BigEndian,
        title: String::from("polydata test"),
        file_path: None,
        data: DataSet::inline(PolyDataPiece {
            points: vec![
                0.0f64, 0.0, 0.0, 1.0, 0.0, 0.0, 0.5, 1.0, 0.0, 0.5, 0.5, 1.0,
            ]
            .into(),
            verts: None,
            lines: None,
            polys: Some(VertexNumbers::Legacy {
                num_cells: 2,
                vertices: vec![3, 0, 1, 2, 3, 1, 2, 3],
            }),
            strips: None,
            data: Attributes {
                point: vec![Attribute::DataArray(DataArrayBase {
                    name: String::from("velocity"),
                    elem: ElementType::Vectors,
                    data: vec![
                        1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0,
                    ]
                    .into(),
                })],
                cell: vec![],
            },
        }),
    };
    vtk.export_ascii(path).unwrap();
}

#[test]
fn vtk_legacy_basic_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mesh.vtk");
    create_unstructured_grid_vtk(&path);

    let meta = VtkExtractor::new().extract(&path).unwrap();

    assert_eq!(meta.format, FileFormat::VtkLegacy);
    assert_eq!(meta.num_rows, Some(3)); // 3 points
    assert!(meta.num_columns.unwrap() > 0);
    assert!(meta
        .dimensions
        .iter()
        .any(|d| d.name == "points" && d.size == Some(3)));
    assert!(meta
        .dimensions
        .iter()
        .any(|d| d.name == "cells" && d.size == Some(1)));
}

#[test]
fn vtk_point_and_cell_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mesh.vtk");
    create_unstructured_grid_vtk(&path);

    let meta = VtkExtractor::new().extract(&path).unwrap();

    // Should have point:pressure and cell:temperature
    assert!(meta.column_names.contains(&"point:pressure".to_string()));
    assert!(meta.column_names.contains(&"cell:temperature".to_string()));

    // Check column types
    let pressure_col = meta
        .columns
        .iter()
        .find(|c| c.name == "point:pressure")
        .unwrap();
    assert_eq!(pressure_col.data_type, fmeta::DataType::Float64);

    let temp_col = meta
        .columns
        .iter()
        .find(|c| c.name == "cell:temperature")
        .unwrap();
    assert_eq!(temp_col.data_type, fmeta::DataType::Float64);
}

#[test]
fn vtk_format_specific() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mesh.vtk");
    create_unstructured_grid_vtk(&path);

    let meta = VtkExtractor::new().extract(&path).unwrap();
    let vtk_meta = match &meta.format_specific {
        Some(FormatMetadata::Vtk(m)) => m,
        other => panic!("expected FormatMetadata::Vtk, got {other:?}"),
    };

    assert_eq!(vtk_meta.dataset_type, "UnstructuredGrid");
    assert_eq!(vtk_meta.num_points, Some(3));
    assert_eq!(vtk_meta.num_cells, Some(1));
    assert_eq!(vtk_meta.version, Some("4.2".to_string()));
    assert_eq!(vtk_meta.title, Some("test mesh".to_string()));
    assert_eq!(vtk_meta.point_data.len(), 1);
    assert_eq!(vtk_meta.point_data[0].name, "pressure");
    assert_eq!(vtk_meta.point_data[0].num_components, 1);
    assert_eq!(vtk_meta.cell_data.len(), 1);
    assert_eq!(vtk_meta.cell_data[0].name, "temperature");
}

#[test]
fn vtk_polydata_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.vtk");
    create_polydata_vtk(&path);

    let meta = VtkExtractor::new().extract(&path).unwrap();
    let vtk_meta = match &meta.format_specific {
        Some(FormatMetadata::Vtk(m)) => m,
        other => panic!("expected FormatMetadata::Vtk, got {other:?}"),
    };

    assert_eq!(vtk_meta.dataset_type, "PolyData");
    assert_eq!(vtk_meta.num_points, Some(4));
    assert_eq!(vtk_meta.num_cells, Some(2));

    // Velocity is a vector (3 components)
    assert_eq!(vtk_meta.point_data[0].name, "velocity");
    assert_eq!(vtk_meta.point_data[0].num_components, 3);
}

#[test]
fn vtk_format_detection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mesh.vtk");
    create_unstructured_grid_vtk(&path);

    let format = detect_format(&path).unwrap();
    assert_eq!(format, FileFormat::VtkLegacy);
}

#[test]
fn vtk_serde_round_trip() {
    let vtk_meta = VtkMetadata {
        version: Some("4.2".into()),
        title: Some("test".into()),
        dataset_type: "UnstructuredGrid".into(),
        num_points: Some(100),
        num_cells: Some(50),
        point_data: vec![],
        cell_data: vec![],
    };
    let fm = FormatMetadata::Vtk(vtk_meta.clone());
    let json = serde_json::to_string(&fm).unwrap();
    let back: FormatMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(fm, back);
}

#[test]
fn vtk_mime_type() {
    assert_eq!(FileFormat::VtkLegacy.mime_type(), "application/x-vtk");
    assert_eq!(FileFormat::Vtu.mime_type(), "application/x-vtk");
    assert_eq!(FileFormat::Vtp.mime_type(), "application/x-vtk");
}

#[test]
fn vtk_file_not_found() {
    let result = VtkExtractor::new().extract(std::path::Path::new("/nonexistent/mesh.vtk"));
    assert!(result.is_err());
}
