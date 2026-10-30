-- Synthetic city offices, each named in its own language and script: a workbook and a PDF per office.
select city, language, lang, dir, greeting
from (values
    (1, '東京', '日本語', 'ja', 'ltr', 'こんにちは'),
    (2, '北京', '中文', 'zh', 'ltr', '你好'),
    (3, '서울', '한국어', 'ko', 'ltr', '안녕하세요'),
    (4, 'Москва', 'Русский', 'ru', 'ltr', 'Здравствуйте'),
    (5, 'القاهرة', 'العربية', 'ar', 'rtl', 'مرحبا'),
    (6, 'Αθήνα', 'Ελληνικά', 'el', 'ltr', 'Γεια σας'),
    (7, 'मुंबई', 'हिन्दी', 'hi', 'ltr', 'नमस्ते'),
    (8, 'São Paulo', 'Português', 'pt', 'ltr', 'Olá'),
    (9, 'Zürich', 'Deutsch', 'de', 'ltr', 'Grüezi')
) as o(office_id, city, language, lang, dir, greeting)
order by office_id
