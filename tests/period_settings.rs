use budget::manager::validate::period_settings;
use tonic::Code;

#[test]
fn accepts_valid_values() {
    let (d, tz) = period_settings(Some(15), Some("Asia/Tokyo")).unwrap();
    assert_eq!(d, Some(15));
    assert_eq!(tz.as_deref(), Some("Asia/Tokyo"));
}

#[test]
fn unset_values_stay_unset() {
    assert_eq!(period_settings(None, None).unwrap(), (None, None));
    assert_eq!(period_settings(None, Some("  ")).unwrap(), (None, None));
}

#[test]
fn rejects_out_of_range_day() {
    for d in [0, 29, 31, -1] {
        assert_eq!(
            period_settings(Some(d), None).unwrap_err().code(),
            Code::InvalidArgument
        );
    }
}

#[test]
fn rejects_unknown_timezone() {
    assert_eq!(
        period_settings(None, Some("Mars/Olympus"))
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
}
