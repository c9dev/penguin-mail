#![cfg(target_os = "linux")]

use std::process::Command;
use std::time::Duration;

use futures::StreamExt;
use ksni::TrayMethods;
use tokio::sync::mpsc;

/// Model the path restriction on Ayatana signals in snapd's unity7 policy.
/// The bus belongs to this test, so it cannot register a tray on the desktop.
#[test]
fn unread_labels_reach_a_host_under_snap_policy() {
    const CHILD: &str = "PENGUIN_MAIL_TEST_PRIVATE_TRAY_BUS";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("bus.conf");
        std::fs::write(
            &config,
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <deny send_type="signal" send_interface="org.kde.StatusNotifierItem"
          send_member="XAyatanaNewLabel" send_path="/StatusNotifierItem"/>
  </policy>
</busconfig>
"#,
        )
        .unwrap();
        let output = Command::new("dbus-run-session")
            .arg(format!("--config-file={}", config.display()))
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "unread_labels_reach_a_host_under_snap_policy",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(check_labels());
}

struct Watcher(mpsc::UnboundedSender<(String, String)>);

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        let (name, path) = if service.starts_with('/') {
            (header.sender().unwrap().to_string(), service.to_string())
        } else {
            (service.to_string(), "/StatusNotifierItem".to_string())
        };
        self.0.send((name, path)).unwrap();
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
}

struct Count(u32);

impl ksni::Tray for Count {
    fn id(&self) -> String {
        "private-tray-test".into()
    }

    fn x_ayatana_label(&self) -> String {
        if self.0 == 0 {
            String::new()
        } else {
            self.0.to_string()
        }
    }

    fn x_ayatana_label_guide(&self) -> String {
        self.x_ayatana_label()
    }
}

async fn check_labels() {
    let (registered, mut registrations) = mpsc::unbounded_channel();
    let host = zbus::connection::Builder::session()
        .name("org.kde.StatusNotifierWatcher")
        .serve_at("/StatusNotifierWatcher", Watcher(registered))
        .build()
        .await
        .unwrap();
    for own_name in [true, false] {
        let tray = Count(1).disable_dbus_name(!own_name).spawn().await.unwrap();
        for restart in [false, true] {
            if restart {
                host.release_name("org.kde.StatusNotifierWatcher")
                    .await
                    .unwrap();
                host.request_name("org.kde.StatusNotifierWatcher")
                    .await
                    .unwrap();
            }
            let (name, path) = tokio::time::timeout(Duration::from_secs(3), registrations.recv())
                .await
                .expect("the tray registers after the panel restarts")
                .unwrap();
            let item = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&host)
                .destination(name.as_str())
                .path(path.as_str())
                .interface("org.kde.StatusNotifierItem")
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
                .unwrap();
            assert_eq!(
                item.get_property::<String>("XAyatanaLabel").await.unwrap(),
                if restart { "" } else { "1" }
            );
            let menu = item
                .get_property::<zbus::OwnedObjectPath>("Menu")
                .await
                .unwrap();
            let menu = zbus::Proxy::new(
                &host,
                name.as_str(),
                menu.as_str(),
                "com.canonical.dbusmenu",
            )
            .await
            .unwrap();
            menu.get_property::<u32>("Version")
                .await
                .expect("the panel can still read the tray menu");
            let mut labels = item.receive_signal("XAyatanaNewLabel").await.unwrap();
            for (count, expected) in [(3, "3"), (0, "")] {
                tray.update(move |tray| tray.0 = count)
                    .await
                    .expect("the tray is running");
                let signal = tokio::time::timeout(Duration::from_secs(3), labels.next())
                    .await
                    .expect("the panel must receive the updated unread label")
                    .unwrap();
                let (label, guide): (String, String) = signal.body().deserialize().unwrap();
                assert_eq!((label.as_str(), guide.as_str()), (expected, expected));
                assert_eq!(
                    item.get_property::<String>("XAyatanaLabel").await.unwrap(),
                    expected
                );
            }
        }
        tray.shutdown().await;
    }
}
