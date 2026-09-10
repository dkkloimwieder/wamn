    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.join(name);
            // A concurrent fork can inherit a writable script descriptor before
            // CLOEXEC closes it, so keep all executable writes in a child process.
            let status = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "umask 077; printf '%s' \"$2\" > \"$1\" && chmod 700 \"$1\"",
                    "operator-fixture",
                ])
                .arg(&path)
                .arg(format!(
                    "#!/bin/sh\ncd '{}' || exit 1\n{body}\n",
                    self.root.display()
                ))
                .status()
                .expect("start the isolated fixture writer");
            assert!(status.success(), "write the executable operator fixture");
            path
        }
    }

