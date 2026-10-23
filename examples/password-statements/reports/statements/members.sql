-- Synthetic members. The password is built in SQL from something the member knows: their date
-- of birth as DDMMYYYY plus their member number. DRE never makes up or stores passwords.
select
    member_id,
    first_name,
    email,
    strftime(date_of_birth, '%d%m%Y') || '-' || member_id as pdf_password
from (values
    (1001, 'Ann', 'ann@example.com', date '1984-03-09'),
    (1002, 'Ben', 'ben@example.com', date '1979-11-23')
) as m(member_id, first_name, email, date_of_birth)
order by member_id
