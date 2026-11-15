-- The close calendar: a date, a timestamp and a time, in the workbook's own formats.
select date '2026-02-03' as close_date,
       timestamp '2026-02-03 17:30:00' as locked_at,
       time '17:30:00' as cutoff
