#![cfg(feature = "zarr")]

use std::sync::Arc;

use fmeta::detect::{detect_format, is_zarr_directory};
use fmeta::extractor::MetadataExtractor;
use fmeta::format::{FileFormat, FormatMetadata, ZarrMetadata};
use fmeta::ZarrExtractor;
use zarrs::array::ArrayBuilder;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;
use zarrs::storage::ReadableWritableListableStorage;

/// Create a V3 Zarr store with a root group and two arrays.
fn create_test_zarr_v3(dir: &std::path::Path) {
    let store: ReadableWritableListableStorage = Arc::new(FilesystemStore::new(dir).unwrap());

    // Root group
    GroupBuilder::new()
        .build(store.clone(), "/")
        .unwrap()
        .store_metadata()
        .unwrap();

    // Array: /temperature — float32 [100, 200]
    let mut temp_array = ArrayBuilder::new(
        vec![100, 200],
        vec![10, 20],
        zarrs::array::DataType::Float32,
        zarrs::array::FillValue::from(0.0f32),
    )
    .dimension_names(Some(vec!["lat".to_string(), "lon".to_string()]))
    .build(store.clone(), "/temperature")
    .unwrap();
    temp_array
        .attributes_mut()
        .insert("units".into(), serde_json::Value::String("kelvin".into()));
    temp_array.store_metadata().unwrap();

    // Array: /pressure — float64 [100, 200]
    ArrayBuilder::new(
        vec![100, 200],
        vec![50, 100],
        zarrs::array::DataType::Float64,
        zarrs::array::FillValue::from(0.0f64),
    )
    .build(store.clone(), "/pressure")
    .unwrap()
    .store_metadata()
    .unwrap();
}

/// Create a V3 Zarr store with nested groups.
fn create_nested_zarr_v3(dir: &std::path::Path) {
    let store: ReadableWritableListableStorage = Arc::new(FilesystemStore::new(dir).unwrap());

    GroupBuilder::new()
        .build(store.clone(), "/")
        .unwrap()
        .store_metadata()
        .unwrap();

    GroupBuilder::new()
        .build(store.clone(), "/experiment1")
        .unwrap()
        .store_metadata()
        .unwrap();

    ArrayBuilder::new(
        vec![50],
        vec![10],
        zarrs::array::DataType::Int32,
        zarrs::array::FillValue::from(0i32),
    )
    .build(store.clone(), "/experiment1/data")
    .unwrap()
    .store_metadata()
    .unwrap();

    GroupBuilder::new()
        .build(store.clone(), "/experiment2")
        .unwrap()
        .store_metadata()
        .unwrap();

    ArrayBuilder::new(
        vec![30, 40],
        vec![10, 10],
        zarrs::array::DataType::UInt8,
        zarrs::array::FillValue::from(0u8),
    )
    .build(store.clone(), "/experiment2/image")
    .unwrap()
    .store_metadata()
    .unwrap();
}

#[test]
fn zarr_v3_basic_metadata() {
    let dir = tempfile::tempdir().unwrap();
    create_test_zarr_v3(dir.path());

    let meta = ZarrExtractor::new().extract(dir.path()).unwrap();

    assert_eq!(meta.format, FileFormat::ZarrV3);
    assert!(meta.num_columns.unwrap() >= 2); // 2 arrays
    assert_eq!(meta.num_rows, Some(100)); // first dimension of first array
}

#[test]
fn zarr_v3_array_shapes() {
    let dir = tempfile::tempdir().unwrap();
    create_test_zarr_v3(dir.path());

    let meta = ZarrExtractor::new().extract(dir.path()).unwrap();
    let zarr_meta = match &meta.format_specific {
        Some(FormatMetadata::Zarr(m)) => m,
        other => panic!("expected FormatMetadata::Zarr, got {other:?}"),
    };

    assert_eq!(zarr_meta.zarr_version, 3);
    assert_eq!(zarr_meta.num_arrays, 2);
    assert!(zarr_meta.num_groups >= 1);

    // Find the temperature array
    let temp_node = zarr_meta
        .hierarchy
        .iter()
        .find(|n| n.path.ends_with("temperature") && n.is_array)
        .expect("temperature array not found");

    let arr_meta = temp_node.array_meta.as_ref().unwrap();
    assert_eq!(arr_meta.shape, vec![100, 200]);
    assert_eq!(arr_meta.data_type, "float32");
    assert_eq!(arr_meta.chunk_shape, vec![10, 20]);

    // Find the pressure array
    let pressure_node = zarr_meta
        .hierarchy
        .iter()
        .find(|n| n.path.ends_with("pressure") && n.is_array)
        .expect("pressure array not found");

    let arr_meta = pressure_node.array_meta.as_ref().unwrap();
    assert_eq!(arr_meta.shape, vec![100, 200]);
    assert_eq!(arr_meta.data_type, "float64");
}

#[test]
fn zarr_v3_dimension_names() {
    let dir = tempfile::tempdir().unwrap();
    create_test_zarr_v3(dir.path());

    let meta = ZarrExtractor::new().extract(dir.path()).unwrap();

    // Dimensions should come from the first array's dimension_names
    assert!(!meta.dimensions.is_empty());

    let zarr_meta = match &meta.format_specific {
        Some(FormatMetadata::Zarr(m)) => m,
        other => panic!("expected FormatMetadata::Zarr, got {other:?}"),
    };

    let temp_node = zarr_meta
        .hierarchy
        .iter()
        .find(|n| n.path.ends_with("temperature") && n.is_array)
        .expect("temperature array not found");

    let arr_meta = temp_node.array_meta.as_ref().unwrap();
    assert_eq!(arr_meta.dimension_names, vec!["lat", "lon"]);
}

#[test]
fn zarr_v3_hierarchy() {
    let dir = tempfile::tempdir().unwrap();
    create_nested_zarr_v3(dir.path());

    let meta = ZarrExtractor::new().extract(dir.path()).unwrap();
    let zarr_meta = match &meta.format_specific {
        Some(FormatMetadata::Zarr(m)) => m,
        other => panic!("expected FormatMetadata::Zarr, got {other:?}"),
    };

    assert_eq!(zarr_meta.num_arrays, 2); // experiment1/data + experiment2/image
    assert!(zarr_meta.num_groups >= 3); // root + experiment1 + experiment2

    // Check we can find nested arrays
    assert!(zarr_meta
        .hierarchy
        .iter()
        .any(|n| n.path.contains("experiment1") && n.path.contains("data") && n.is_array));
    assert!(zarr_meta
        .hierarchy
        .iter()
        .any(|n| n.path.contains("experiment2") && n.path.contains("image") && n.is_array));
}

#[test]
fn zarr_v3_serde_round_trip() {
    use fmeta::format::{ZarrArrayMeta, ZarrNode};

    let zarr_meta = ZarrMetadata {
        zarr_version: 3,
        num_arrays: 1,
        num_groups: 1,
        hierarchy: vec![ZarrNode {
            path: "/array".into(),
            is_array: true,
            array_meta: Some(ZarrArrayMeta {
                shape: vec![10, 20],
                data_type: "float32".into(),
                chunk_shape: vec![5, 10],
                codecs: vec!["bytes".into()],
                fill_value: Some("0".into()),
                dimension_names: vec!["x".into(), "y".into()],
            }),
            attributes: vec![],
        }],
    };
    let fm = FormatMetadata::Zarr(zarr_meta.clone());
    let json = serde_json::to_string(&fm).unwrap();
    let back: FormatMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(fm, back);
}

#[test]
fn zarr_v3_mime_type() {
    assert_eq!(FileFormat::ZarrV2.mime_type(), "application/x-zarr");
    assert_eq!(FileFormat::ZarrV3.mime_type(), "application/x-zarr");
}

#[test]
fn zarr_detection_directory() {
    let dir = tempfile::tempdir().unwrap();
    create_test_zarr_v3(dir.path());

    let format = detect_format(dir.path()).unwrap();
    assert_eq!(format, FileFormat::ZarrV3);
    assert!(is_zarr_directory(dir.path()));
}

#[test]
fn zarr_nonexistent_path() {
    let result = ZarrExtractor::new().extract(std::path::Path::new("/nonexistent/store.zarr"));
    assert!(result.is_err());
}
