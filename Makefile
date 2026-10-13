test:
	python3 -m unittest discover -s tests -v
	python3 -m compileall -q cloud_contacts
	bash -n install.sh packaging/PKGBUILD

native-test:
	python3 tests/native_smoke.py
