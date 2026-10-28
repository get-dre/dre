-- Synthetic city offices, each named in its own language and script: one workbook per office.
select city, language, greeting
from (values
    (1, '東京', '日本語', 'こんにちは'),
    (2, '北京', '中文', '你好'),
    (3, '서울', '한국어', '안녕하세요'),
    (4, 'Москва', 'Русский', 'Здравствуйте'),
    (5, 'القاهرة', 'العربية', 'مرحبا'),
    (6, 'Αθήνα', 'Ελληνικά', 'Γεια σας'),
    (7, 'मुंबई', 'हिन्दी', 'नमस्ते'),
    (8, 'São Paulo', 'Português', 'Olá'),
    (9, 'Zürich', 'Deutsch', 'Grüezi')
) as o(office_id, city, language, greeting)
order by office_id
