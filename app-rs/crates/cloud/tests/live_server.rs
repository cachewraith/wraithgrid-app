//! The desktop's calls against a running wraithgrid-server. Opt-in (`--ignored`).
//!
//! In-app email account (needs a confirmed account; changes its password and back):
//!   WRAITHGRID_TEST_SERVER=http://localhost:3000 WG_TEST_EMAIL=… WG_TEST_PASSWORD=… \
//!     cargo test -p wraithgrid-cloud --test live_server -- --ignored --test-threads=1
//! Safe against production (creates nothing):
//!   WRAITHGRID_TEST_SERVER=https://api.wraithgrid.cachewraith.com \
//!     cargo test -p wraithgrid-cloud --test live_server public_endpoints -- --ignored

use wraithgrid_cloud::{Client, DeviceInfo};

fn device(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: wraithgrid_cloud::platform().into(),
        app_version: "2.3.0".into(),
    }
}

fn account() -> (Client, String, String) {
    let (Ok(server), Ok(email), Ok(password)) = (
        std::env::var("WRAITHGRID_TEST_SERVER"),
        std::env::var("WG_TEST_EMAIL"),
        std::env::var("WG_TEST_PASSWORD"),
    ) else {
        panic!("set WRAITHGRID_TEST_SERVER, WG_TEST_EMAIL and WG_TEST_PASSWORD");
    };
    (Client::live(server, "2.3.0".into()), email, password)
}

#[test]
#[ignore = "needs a running wraithgrid-server"]
fn email_sign_in_profile_and_sign_out() {
    let (app, email, password) = account();
    let wrong = app
        .email_sign_in(&email, "not the password", &device("e2e"))
        .unwrap_err();
    assert_eq!(
        (wrong.status, wrong.code.as_str()),
        (401, "invalid_credentials")
    );
    assert_eq!(wrong.message, "Wrong email or password");

    let grant = app
        .email_sign_in(&email, &password, &device("e2e"))
        .unwrap();
    assert_eq!(grant.user["email"], email.to_lowercase());
    assert_eq!(grant.user["providers"][0], "email");
    let other = app
        .email_sign_in(&email, &password, &device("e2e-other"))
        .unwrap();

    let renamed = app.update_name(&grant.token, "E2E Renamed").unwrap();
    assert_eq!(renamed["name"], "E2E Renamed");

    // Changing the password signs the other device out; this one stays. Then back again.
    let temp = format!("{password}-temp-change");
    let e = app
        .change_password(&grant.token, "not the password", &temp)
        .unwrap_err();
    assert_eq!(e.code, "wrong_password");
    app.change_password(&grant.token, &password, &temp).unwrap();
    assert!(app.me(&other.token).unwrap_err().is_signed_out());
    assert_eq!(app.me(&grant.token).unwrap()["email"], email.to_lowercase());
    app.change_password(&grant.token, &temp, &password).unwrap();

    // Codes: same answer for unknown emails; a wrong code is refused.
    app.email_resend(&email).unwrap();
    app.email_resend("nobody-here@example.com").unwrap();
    app.email_reset("nobody-here@example.com").unwrap();
    let bad = app
        .email_reset_confirm(
            "nobody-here@example.com",
            "123456",
            "a long password!",
            &device("x"),
        )
        .unwrap_err();
    assert_eq!(bad.code, "invalid_code");
    let bad = app
        .email_verify("nobody-here@example.com", "123456", &device("x"))
        .unwrap_err();
    assert_eq!(bad.code, "invalid_code");

    app.revoke_device(&grant.token, &grant.device_id).unwrap();
    assert!(app.me(&grant.token).unwrap_err().is_signed_out());
}

/// Safe against production: creates nothing (config + a refused sign-in).
#[test]
#[ignore = "needs a running wraithgrid-server"]
fn public_endpoints() {
    let server = std::env::var("WRAITHGRID_TEST_SERVER").expect("set WRAITHGRID_TEST_SERVER");
    let app = Client::live(server, env!("CARGO_PKG_VERSION").into());
    let config = app.config().unwrap();
    assert_eq!(config["readOnly"], false);
    let e = app
        .email_sign_in(
            "probe@example.invalid",
            "not-a-real-password",
            &device("smoke-test"),
        )
        .unwrap_err();
    assert_eq!((e.status, e.code.as_str()), (401, "invalid_credentials"));
}
