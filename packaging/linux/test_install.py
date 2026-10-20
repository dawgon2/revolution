#!/usr/bin/env python3
"""Exercise the tarball installer with disposable binaries and installation directories."""

import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time
import unittest


HERE = Path(__file__).resolve().parent
APP_ID = "io.github.dawgon2.Revolution"


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="filmcraft-install-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bundle = self.root / "extracted bundle"
        (self.bundle / "bin").mkdir(parents=True)
        self.share = self.bundle / "share"
        (self.share / "applications").mkdir(parents=True)
        shutil.copyfile(HERE / "install.sh", self.bundle / "install.sh")
        shutil.copyfile(
            HERE / f"{APP_ID}.desktop", self.share / "applications" / f"{APP_ID}.desktop"
        )
        for name in ("revolution", "revolution-cli"):
            (self.bundle / "bin" / name).write_text(
                '#!/bin/sh\nprintf \'%s\\n\' "$0" "$@" > "$(dirname "$0")/launched"\n'
            )
        for path in ("doc/revolution/LICENSE-MIT", "icons/hicolor/test", "mime/packages/test.xml"):
            target = self.share / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("fixture\n")

    def install(self, *args):
        # Run outside the extracted directory to check that the script finds its own files.
        return subprocess.run(
            ["bash", str(self.bundle / "install.sh"), *map(str, args)],
            cwd=self.root, capture_output=True, text=True, timeout=30,
        )

    def test_install_and_update_preserve_other_applications(self):
        prefix = self.root / "user programs"
        other = prefix / "share/applications/other.desktop"
        other.parent.mkdir(parents=True)
        other.write_text("unrelated\n")
        for version in ("first", "updated"):
            (self.bundle / "share/doc/revolution/LICENSE-MIT").write_text(version)
            result = self.install("--prefix", prefix)
            self.assertEqual(result.returncode, 0, result.stderr)
            for name in ("revolution", "revolution-cli"):
                self.assertTrue(os.access(prefix / "bin" / name, os.X_OK))
                self.assertEqual(
                    (prefix / "bin" / name).read_bytes(), (self.bundle / "bin" / name).read_bytes()
                )
            self.assertEqual((prefix / "share/doc/revolution/LICENSE-MIT").read_text(), version)
            self.assertTrue((prefix / "share/icons/hicolor/test").is_file())
            self.assertTrue((prefix / "share/mime/packages/test.xml").is_file())
            self.assertEqual(other.read_text(), "unrelated\n")
        desktop = prefix / "share/applications" / f"{APP_ID}.desktop"
        self.assertIn(f'Exec="{prefix}/bin/revolution" %F\n', desktop.read_text())
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", str(desktop)], check=True)

    @unittest.skipUnless(shutil.which("gio"), "gio is needed to exercise the desktop launcher")
    def test_desktop_launch_escapes_paths_and_preserves_file_arguments(self):
        prefix = self.root / 'programs with "quotes" $dollar `tick` \\slash & more'
        result = self.install("--prefix", prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        document = self.root / "my film.filmcraft"
        document.touch()
        desktop = prefix / "share/applications" / f"{APP_ID}.desktop"
        subprocess.run(["gio", "launch", str(desktop), str(document)], check=True, timeout=10)
        launched = prefix / "bin/launched"
        for _ in range(100):
            if launched.exists() and launched.read_text().splitlines() == [
                str(prefix / "bin/revolution"), str(document)
            ]:
                break
            time.sleep(0.01)
        self.assertEqual(launched.read_text().splitlines(), [str(prefix / "bin/revolution"), str(document)])

    def test_actions_keep_their_arguments_and_thumbnailer_gets_absolute_path(self):
        thumb = self.share / "thumbnailers" / f"{APP_ID}.thumbnailer"
        thumb.parent.mkdir(parents=True)
        shutil.copyfile(HERE / f"{APP_ID}.thumbnailer", thumb)
        prefix = self.root / "user programs"
        result = self.install("--prefix", prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        desktop = (prefix / "share/applications" / f"{APP_ID}.desktop").read_text()
        self.assertIn(f'Exec="{prefix}/bin/revolution" %F\n', desktop)
        self.assertIn(f'Exec="{prefix}/bin/revolution" --empty\n', desktop)
        self.assertIn(f'Exec="{prefix}/bin/revolution" --new-instance\n', desktop)
        self.assertIn("MimeType=application/x-filmcraft-project;", desktop)
        installed = (prefix / "share/thumbnailers" / f"{APP_ID}.thumbnailer").read_text()
        self.assertIn(f'Exec="{prefix}/bin/revolution-cli" thumbnail %i %s %o\n', installed)
        self.assertIn(f"TryExec={prefix}/bin/revolution-cli\n", installed)
        if shutil.which("desktop-file-validate"):
            subprocess.run(
                ["desktop-file-validate", str(prefix / "share/applications" / f"{APP_ID}.desktop")],
                check=True,
            )

    def test_invalid_arguments_fail_before_writing(self):
        for args in (("--prefix",), ("--prefix", "relative"), ("--prefix", "/"), ("--unknown",),
                     ("--prefix", "///"), ("--prefix", "/tmp/../"), ("--prefix", str(self.bundle)),
                     ("--prefix", str(self.bundle / "nested")),
                     ("--prefix", str(self.root / "bad=path")),
                     ("--prefix", str(self.root / "bad%path")),
                     ("--prefix", str(self.root / "bad\npath"))):
            with self.subTest(args=args):
                self.assertNotEqual(self.install(*args).returncode, 0)

    def test_incomplete_bundle_does_not_install(self):
        (self.bundle / "bin/revolution-cli").unlink()
        prefix = self.root / "destination"
        result = self.install("--prefix", prefix)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("extract the complete tarball", result.stderr)
        self.assertFalse(prefix.exists())

    def test_help(self):
        self.assertEqual(self.install("--help").returncode, 0)

    def package(self, formats, **env):
        """Runs package.sh --skip-build on fixture binaries (cargo names them filmcraft /
        filmcraft-cli; the packages install them as revolution / revolution-cli)."""
        target = self.root / "target"
        binaries = target / "release"
        binaries.mkdir(parents=True, exist_ok=True)
        for name in ("filmcraft", "filmcraft-cli"):
            binary = binaries / name
            binary.write_text("#!/bin/sh\necho 'FilmCraft fixture'\n")
            binary.chmod(0o755)
        dist = self.root / "dist"
        result = subprocess.run(
            ["bash", str(HERE / "package.sh"), "--skip-build", "--formats", formats],
            env={**os.environ, "CARGO_TARGET_DIR": str(target), "DIST": str(dist), **env},
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return dist

    def test_release_tarball_includes_executable_installer(self):
        dist = self.package("tar")
        archives = list(dist.glob("*-fhs.tar.gz"))
        self.assertEqual(len(archives), 1)
        with tarfile.open(archives[0]) as archive:
            installer = next(m for m in archive.getmembers() if m.name.endswith("/install.sh"))
            self.assertEqual(installer.mode & 0o777, 0o755)
            self.assertEqual(archive.extractfile(installer).read(), (HERE / "install.sh").read_bytes())
            names = archive.getnames()
            self.assertTrue(any(n.endswith("/bin/revolution") for n in names), names)
            self.assertTrue(any(n.endswith("/bin/revolution-cli") for n in names), names)

    def test_portable_tarball_has_the_updaters_layout(self):
        """The updater (crates/engine/src/update_install.rs) unpacks `revolution-<ver>/` holding
        `revolution`; the version comes from FILMCRAFT_RELEASE_TAG (FORK-PLAN §1a.107)."""
        notes = self.root / "NOTES.md"
        notes.write_text("# Revolution 9.8.7 notes\n")
        dist = self.package("portable", FILMCRAFT_RELEASE_TAG="revolution-9.8.7", REVOLUTION_NOTES=str(notes))
        archives = list(dist.glob("*.tar.gz"))
        self.assertEqual([a.name for a in archives], [f"revolution-9.8.7-linux-{os.uname().machine}.tar.gz"])
        with tarfile.open(archives[0]) as archive:
            names = set(archive.getnames())
            for want in ("revolution", "revolution-cli", "NOTES.md", "LICENSE-MIT"):
                self.assertIn(f"revolution-9.8.7/{want}", names)
            self.assertEqual(archive.getmember("revolution-9.8.7/revolution").mode & 0o777, 0o755)
            self.assertEqual(archive.extractfile("revolution-9.8.7/NOTES.md").read(), notes.read_bytes())

    def test_portable_tarball_follows_a_two_number_tag(self):
        """revolution-0.2 packages as version 0.2.0 (nfpm / MSI), but the updater wants the tag."""
        dist = self.package("portable", FILMCRAFT_RELEASE_TAG="revolution-0.2", FILMCRAFT_VERSION="0.2.0")
        archives = list(dist.glob("*.tar.gz"))
        self.assertEqual([a.name for a in archives], [f"revolution-0.2-linux-{os.uname().machine}.tar.gz"])
        with tarfile.open(archives[0]) as archive:
            self.assertIn("revolution-0.2/revolution", archive.getnames())


if __name__ == "__main__":
    unittest.main()
