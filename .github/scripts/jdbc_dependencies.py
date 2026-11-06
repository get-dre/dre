#!/usr/bin/env python3
"""Review/check JDBC runtime dependencies: exact jars, SHA-256 and GPL-compatible licences.

Run `jdbc_dependencies.py sync` after a dependency update, then review dependencies.lock.json.
MAVEN and MAVEN_REPO_LOCAL optionally select a local Maven executable/cache.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
LOCK = ROOT / 'java/jdbc/dependencies.lock.json'
# Maven POM licence spellings in the reviewed dependency tree.
LICENSES = {'Apache License V2.0', 'Apache-2.0', 'Apache License, Version 2.0', 'The Apache Software License, Version 2.0',
            'Apache License 2.0', 'Apache 2.0', 'MIT License', 'The MIT License', 'MIT',
            'BSD-3-Clause', 'BSD licence', 'BSD License'}


def inventory(report, jars):
    licenses = {}
    for dependency in ET.parse(report).findall('.//dependency'):
        coordinate = ':'.join(dependency.findtext(k, '') for k in ('groupId', 'artifactId', 'version'))
        names = sorted({n.text for n in dependency.findall('licenses/license/name')})
        if not names or not any(n in LICENSES for n in names):
            raise ValueError(f'{coordinate}: missing or unapproved licence {names}')
        licenses[dependency.findtext('artifactId') + '-' + dependency.findtext('version') + '.jar'] = (coordinate, names)
    entries = []
    for jar in jars:
        jar = Path(jar)
        if jar.name not in licenses:
            raise ValueError(f'{jar.name}: no licence metadata')
        coordinate, names = licenses[jar.name]
        entries.append({'artifact': coordinate, 'licenses': names, 'sha256': hashlib.sha256(jar.read_bytes()).hexdigest()})
    if not entries:
        raise ValueError('no JDBC dependencies resolved')
    return sorted(entries, key=lambda e: e['artifact'])


def check(actual, locked):
    if actual != locked:
        raise ValueError('JDBC dependencies differ from the reviewed lock; run jdbc_dependencies.py sync and review')


def main(mode):
    with tempfile.TemporaryDirectory(prefix='dre-jdbc-deps-') as directory:
        directory = Path(directory)
        report, classpath = directory / 'licenses.xml', directory / 'classpath.txt'
        command = [os.environ.get('MAVEN', 'mvn'), '-B', '-C', '-f', str(ROOT / 'java/jdbc/pom.xml')]
        if os.environ.get('MAVEN_REPO_LOCAL'):
            command.append('-Dmaven.repo.local=' + os.environ['MAVEN_REPO_LOCAL'])
        subprocess.run(command + ['org.apache.maven.plugins:maven-dependency-plugin:3.9.0:build-classpath',
                                 '-Dmdep.outputFile=' + str(classpath), '-DincludeScope=runtime',
                                 'org.codehaus.mojo:license-maven-plugin:2.7.0:download-licenses',
                                 '-DlicensesOutputFile=' + str(report),
                                 '-DlicensesOutputDirectory=' + str(directory / 'licenses'),
                                 '-Dlicense.excludedScopes=test,provided,system'], check=True)
        actual = inventory(report, classpath.read_text().strip().split(os.pathsep))
        if mode == 'sync':
            LOCK.write_text(json.dumps(actual, indent=2) + '\n')
        elif mode == 'check':
            check(actual, json.loads(LOCK.read_text()))
        else:
            raise ValueError('expected check or sync')


if __name__ == '__main__':
    main(*sys.argv[1:])
