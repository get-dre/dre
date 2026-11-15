-- Every office's greeting, for one notice in every script.
select city, greeting, lang, dir
from (values
    (1, '東京', 'こんにちは', 'ja', 'ltr'),
    (2, '서울', '안녕하세요', 'ko', 'ltr'),
    (3, 'Москва', 'Здравствуйте', 'ru', 'ltr'),
    (4, 'القاهرة', 'مرحبا', 'ar', 'rtl'),
    (5, 'Αθήνα', 'Γεια σας', 'el', 'ltr'),
    (6, 'São Paulo', 'Olá', 'pt', 'ltr')
) as o(office_id, city, greeting, lang, dir)
order by office_id
