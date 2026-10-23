-- The file -> password list, for the client to keep. Never sent with the files.
select member_id, strftime(date_of_birth, '%d%m%Y') || '-' || member_id as pdf_password
from (values (1001, date '1984-03-09'), (1002, date '1979-11-23')) as m(member_id, date_of_birth)
order by member_id
