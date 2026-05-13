//! Integration tests for NetCDF and HDF5 backends via the `netcdf` crate.

#[cfg(feature = "netcdf")]
mod netcdf_tests {
    use fmeta::{extract_metadata, DataType, FileFormat, FormatMetadata};

    /// Create a NetCDF classic file with dimensions and variables.
    fn create_netcdf_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".nc").unwrap();
        let mut file = netcdf::create(tmp.path()).unwrap();
        file.add_dimension("x", 10).unwrap();
        file.add_dimension("y", 5).unwrap();
        let mut temp = file
            .add_variable::<f64>("temperature", &["x", "y"])
            .unwrap();
        let data: Vec<f64> = (0..50).map(|i| i as f64 * 0.1).collect();
        temp.put_values(&data, ..).unwrap();
        file.add_variable::<i32>("station_id", &["x"]).unwrap();
        file.close().unwrap();
        tmp
    }

    /// Create a NetCDF4 (HDF5-based) file with groups.
    fn create_netcdf4_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".nc").unwrap();
        let mut file = netcdf::create_with(tmp.path(), netcdf::Options::NETCDF4).unwrap();
        file.add_dimension("time", 3).unwrap();
        file.add_variable::<f32>("global_temp", &["time"]).unwrap();
        file.add_attribute("Conventions", "CF-1.8").unwrap();
        file.add_attribute("title", "Test dataset").unwrap();

        let mut grp = file.add_group("measurements").unwrap();
        grp.add_dimension("sensor", 2).unwrap();
        grp.add_variable::<f64>("pressure", &["sensor"]).unwrap();

        file.close().unwrap();
        tmp
    }

    /// Create an HDF5 file (NetCDF4 format) with .h5 extension.
    fn create_hdf5_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".h5").unwrap();
        let mut file = netcdf::create_with(tmp.path(), netcdf::Options::NETCDF4).unwrap();
        file.add_dimension("rows", 100).unwrap();
        file.add_variable::<f64>("data", &["rows"]).unwrap();
        file.add_attribute("author", "test").unwrap();

        let mut grp = file.add_group("sensors").unwrap();
        grp.add_dimension("n", 5).unwrap();
        grp.add_variable::<i32>("ids", &["n"]).unwrap();
        grp.add_attribute("description", "Sensor IDs").unwrap();

        let mut sub = grp.add_group("calibration").unwrap();
        sub.add_variable::<f32>("offsets", &["n"]).unwrap();

        file.close().unwrap();
        tmp
    }

    // ========================================================================
    // NetCDF tests
    // ========================================================================

    #[test]
    fn netcdf_basic_metadata() {
        let tmp = create_netcdf_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::NetCdf);
        assert_eq!(meta.num_columns, Some(2));
        assert!(meta.num_rows.is_some());
        assert_eq!(meta.column_names, vec!["temperature", "station_id"]);
    }

    #[test]
    fn netcdf_variable_types() {
        let tmp = create_netcdf_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        let temp_col = meta
            .columns
            .iter()
            .find(|c| c.name == "temperature")
            .unwrap();
        assert_eq!(temp_col.data_type, DataType::Float64);

        let id_col = meta
            .columns
            .iter()
            .find(|c| c.name == "station_id")
            .unwrap();
        assert_eq!(id_col.data_type, DataType::Int32);
    }

    #[test]
    fn netcdf_dimensions_populated() {
        let tmp = create_netcdf_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.dimensions.len(), 2);
        let x_dim = meta.dimensions.iter().find(|d| d.name == "x").unwrap();
        assert_eq!(x_dim.size, Some(10));
        let y_dim = meta.dimensions.iter().find(|d| d.name == "y").unwrap();
        assert_eq!(y_dim.size, Some(5));
    }

    #[test]
    fn netcdf_global_attributes() {
        let tmp = create_netcdf4_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        let title = meta.attributes.get("title").unwrap();
        assert_eq!(
            title,
            &fmeta::AttributeValue::String("Test dataset".into())
        );
    }

    #[test]
    fn netcdf_format_specific() {
        let tmp = create_netcdf4_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::NetCdf(nc_meta)) => {
                assert_eq!(nc_meta.conventions.as_deref(), Some("CF-1.8"));
                assert!(nc_meta.variables.contains(&"global_temp".to_string()));
            }
            other => panic!("Expected NetCdf format_specific, got {other:?}"),
        }
    }

    #[test]
    fn netcdf_serde_round_trip() {
        let tmp = create_netcdf_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_columns, back.num_columns);
        assert_eq!(meta.column_names, back.column_names);
        assert_eq!(meta.dimensions.len(), back.dimensions.len());
    }

    #[test]
    fn netcdf_mime_type() {
        let tmp = create_netcdf_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.mime_type.as_deref(), Some("application/x-netcdf"));
    }

    // ========================================================================
    // HDF5 tests
    // ========================================================================

    #[test]
    fn hdf5_basic_metadata() {
        let tmp = create_hdf5_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::Hdf5);
        assert_eq!(meta.mime_type.as_deref(), Some("application/x-hdf5"));
        // Root "data" + /sensors/ids + /sensors/calibration/offsets = 3 columns
        assert_eq!(meta.num_columns, Some(3));
    }

    #[test]
    fn hdf5_group_hierarchy() {
        let tmp = create_hdf5_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Hdf5(hdf_meta)) => {
                // Root + /sensors + /sensors/calibration = 3 groups
                assert!(hdf_meta.groups.len() >= 3);

                let root = hdf_meta.groups.iter().find(|g| g.path == "/").unwrap();
                assert!(root.datasets.contains(&"data".to_string()));

                let sensors = hdf_meta
                    .groups
                    .iter()
                    .find(|g| g.path == "/sensors")
                    .unwrap();
                assert!(sensors.datasets.contains(&"ids".to_string()));

                let cal = hdf_meta
                    .groups
                    .iter()
                    .find(|g| g.path == "/sensors/calibration")
                    .unwrap();
                assert!(cal.datasets.contains(&"offsets".to_string()));
            }
            other => panic!("Expected Hdf5 format_specific, got {other:?}"),
        }
    }

    #[test]
    fn hdf5_dataset_types() {
        let tmp = create_hdf5_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        let data_col = meta.columns.iter().find(|c| c.name == "data").unwrap();
        assert_eq!(data_col.data_type, DataType::Float64);

        let ids_col = meta
            .columns
            .iter()
            .find(|c| c.name.ends_with("/ids"))
            .unwrap();
        assert_eq!(ids_col.data_type, DataType::Int32);

        let offsets_col = meta
            .columns
            .iter()
            .find(|c| c.name.ends_with("/offsets"))
            .unwrap();
        assert_eq!(offsets_col.data_type, DataType::Float32);
    }

    #[test]
    fn hdf5_group_attributes() {
        let tmp = create_hdf5_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        // Root attribute
        let author = meta.attributes.get("author").unwrap();
        assert_eq!(
            author,
            &fmeta::AttributeValue::String("test".into())
        );

        // Group-level attribute
        match &meta.format_specific {
            Some(FormatMetadata::Hdf5(hdf_meta)) => {
                let sensors = hdf_meta
                    .groups
                    .iter()
                    .find(|g| g.path == "/sensors")
                    .unwrap();
                let desc = sensors
                    .attributes
                    .iter()
                    .find(|(k, _)| k == "description")
                    .unwrap();
                assert_eq!(
                    desc.1,
                    fmeta::AttributeValue::String("Sensor IDs".into())
                );
            }
            other => panic!("Expected Hdf5, got {other:?}"),
        }
    }

    #[test]
    fn hdf5_serde_round_trip() {
        let tmp = create_hdf5_file();
        let meta = extract_metadata(tmp.path()).unwrap();

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_columns, back.num_columns);
        match (&meta.format_specific, &back.format_specific) {
            (Some(FormatMetadata::Hdf5(a)), Some(FormatMetadata::Hdf5(b))) => {
                assert_eq!(a.groups.len(), b.groups.len());
            }
            _ => panic!("format_specific mismatch after round-trip"),
        }
    }

    #[test]
    fn hdf5_file_not_found() {
        let result = extract_metadata(std::path::Path::new("/nonexistent/file.h5"));
        assert!(result.is_err());
    }

    // ========================================================================
    // Format detection
    // ========================================================================

    #[test]
    fn netcdf4_with_nc_extension_detected_as_netcdf() {
        // NetCDF4 files use HDF5 magic, but .nc extension should win
        let tmp = create_netcdf4_file();
        let format = fmeta::detect_format(tmp.path()).unwrap();
        assert_eq!(format, FileFormat::NetCdf);
    }

    #[test]
    fn hdf5_with_h5_extension_detected_as_hdf5() {
        let tmp = create_hdf5_file();
        let format = fmeta::detect_format(tmp.path()).unwrap();
        assert_eq!(format, FileFormat::Hdf5);
    }
}
