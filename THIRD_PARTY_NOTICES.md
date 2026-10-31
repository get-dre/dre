# Third-party notices

DRE is licensed under GPL-3.0-only. The first-party JDBC plugin combines DRE's
own Java code with the components below. Each remains under its own license.

## JDBC plugin JAR

| Component | Version | License |
|---|---|---|
| Apache Arrow Java (JDBC adapter, vector, memory, format) | 19.0.0 | Apache-2.0 |
| Jackson (databind, core, annotations, JSR-310) | 2.21.x | Apache-2.0 |
| SLF4J API and NOP provider | 2.0.17 | MIT |
| Netty (buffer and common) | 4.2.9.Final | Apache-2.0 |
| FlatBuffers Java | 25.2.10 | Apache-2.0 |
| Apache Commons Codec | 1.21.0 | Apache-2.0 |

The shaded JAR includes the license and notice material those components
distribute. Vendor JDBC drivers supplied by users are not part of this package
and keep their own licenses.

## Bundled Java runtime

The package includes an Eclipse Temurin JDK subset, version 17.0.20.1+1
(OpenJDK 17.0.20+101), produced with `jlink`. Its `legal/` directory contains
the per-module licenses and notices from that exact Temurin build, including
the GNU General Public License version 2 with the Classpath Exception and
other component notices. Those files are authoritative for the runtime.

No copyright owner is claimed here for third-party material. Names and
trademarks belong to their respective owners.
