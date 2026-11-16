//! Process-wide initialization and shutdown, in a process of its own.

mod support;

use std::{sync::atomic::Ordering, thread, time::Duration};

use connector_api::abi::*;
use connector_sdk::test_connector;
use serde_json::Value;
use support::*;

fn initialize_library() -> Result<(), Error> {
    let directory = text("/opt/adbc");
    let settings = ScalardbAnalyticsConnectorLibrarySettings {
        adbc_driver_directory: directory.as_ptr(),
        odbc_driver_directory: std::ptr::null(),
    };
    // SAFETY: the settings are valid for the call.
    check(unsafe { (api().library_initialize)(&settings) })
}

#[test]
fn the_library_is_initialized_once_and_shut_down_by_the_application() {
    let information: Value =
        serde_json::from_str(&read_string((api().static_information)()).unwrap()).unwrap();
    assert_eq!(information["static_information_version"], 1);
    assert_eq!(information["library_version"], env!("CARGO_PKG_VERSION"));
    let connector = &information["connectors"][0];
    assert_eq!(connector["name"], "test");
    assert_eq!(connector["connectors"][0], "test");
    assert_eq!(connector["data_source_access"]["form"], "direct");
    let token = connector["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["key"] == "token")
        .unwrap();
    assert_eq!(token["secret"], true);
    assert_eq!(token["required"], false);
    assert_eq!(token["value_type"], "string");

    let error = DataSource::open(&[("label", "before")]).err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );

    assert!(!(api().library_is_initialized)());
    initialize_library().unwrap();
    assert!((api().library_is_initialized)());
    let error = initialize_library().err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    let data_source = DataSource::open(&[("label", "lifecycle"), ("token", "hunter2")]).unwrap();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["block"]);
    let Read {
        operation,
        mut stream,
    } = session.read(&remote, 50).unwrap();
    let probe = test_connector::probe("lifecycle");

    // A read already blocked at the source still stops at its deadline after
    // the application shuts the library down.
    let pulled = thread::scope(|scope| {
        let puller = scope.spawn(move || stream.pull());
        while probe.sent.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(5));
        }
        check((api().library_shutdown)()).unwrap();
        puller.join().unwrap()
    });
    assert!(!(api().library_is_initialized)());

    assert!(matches!(pulled, Pull::Failed(..)));
    assert_eq!(
        operation.error().unwrap().kind,
        ScalardbAnalyticsConnectorErrorKind::Timeout
    );
    operation.close();

    let error = session.plan(&["batches", "1"]).err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    let error = data_source.session().err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    let error = check((api().library_shutdown)()).err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    session.close();
    data_source.close();

    initialize_library().unwrap();
    assert!((api().library_is_initialized)());
    assert!(DataSource::open(&[("label", "reinitialized")]).is_ok());
}
