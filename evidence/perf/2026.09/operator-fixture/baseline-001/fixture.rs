    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\ncd '{}' || exit 1\n{body}\n",
                    self.root.display()
                ),
            )
            .expect("write the operator fixture");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("make the fixture executable");
            path
        }
    }

