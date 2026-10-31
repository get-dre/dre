# Third-party notices

DRE is licensed under GPL-3.0-only. The first-party JDBC plugin combines DRE's
own Java code with the components below. Each remains under its own license.

## JDBC plugin JAR

| Component | Version | License |
|---|---|---|
| Apache Arrow Java (JDBC adapter, vector, memory, format) | 19.0.0 | Apache-2.0 |
| Jackson core, databind and JSR-310 | 2.22.4 | Apache-2.0 |
| Jackson annotations | 2.22 | Apache-2.0 |
| SLF4J API and NOP provider | 2.0.20 | MIT |
| Netty (buffer and common) | 4.2.9.Final | Apache-2.0 |
| FlatBuffers Java | 25.2.10 | Apache-2.0 |
| Apache Commons Codec | 1.21.0 | Apache-2.0 |
| JCTools | 4.0.5 | Apache-2.0 |

The shaded JAR includes the license and notice material those components
distribute. Vendor JDBC drivers supplied by users are not part of this package
and keep their own licenses.

Java runtimes are not included, downloaded or updated by this package. Operators
supply a compatible Java 17 or newer runtime under its own license.

No copyright owner is claimed here for third-party material. Names and
trademarks belong to their respective owners.
