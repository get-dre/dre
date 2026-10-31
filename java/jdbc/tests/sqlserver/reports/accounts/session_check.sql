select @@datefirst as session_setting,
       cast(@@spid as varchar(20)) as session_id,
       convert(varchar(10), cast('31/01/2026' as date), 111) as prepared_date;
