`branded.xlsx` covers basic template filling. `reporting.xlsx` extends it with ordinary
reporting features and was opened and saved in Microsoft Excel for Mac. Personal author
metadata and the local source path were removed before committing.

On Summary, A1:C1 is a merged title, the logo sits to its right, row 4 contains headings,
and row 5 is the reserved data row. A5:C5 has wrapped text and a 32-point row height.
D5 contains `=C5*2`; C6 totals `=SUM(C5:C5)`. Panes freeze the first four rows and column A.
The filter is A4:C5. C5 has a cell-value rule highlighting amounts greater than 150.
Printing uses landscape A4, print area A1:D8 and repeating rows 1:4.

The integration tests fill empty, single-row and expanded results through the plugin protocol.
They inspect the saved workbook, including ranges and formulas rather than calculated caches.
The fixture contains synthetic data only.
