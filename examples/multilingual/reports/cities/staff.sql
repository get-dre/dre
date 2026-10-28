-- Synthetic staff, with names and roles in each office's language.
select city, name, role
from (values
    ('東京', '佐藤 花子', '支店長'),
    ('東京', '鈴木 一郎', '経理'),
    ('北京', '王 伟', '经理'),
    ('北京', '李 娜', '会计'),
    ('서울', '김 민준', '지점장'),
    ('서울', '이 서연', '회계'),
    ('Москва', 'Иван Петров', 'Директор'),
    ('Москва', 'Анна Смирнова', 'Бухгалтер'),
    ('القاهرة', 'أحمد حسن', 'مدير'),
    ('القاهرة', 'فاطمة علي', 'محاسبة'),
    ('Αθήνα', 'Γιώργος Παπαδόπουλος', 'Διευθυντής'),
    ('Αθήνα', 'Μαρία Νικολάου', 'Λογίστρια'),
    ('मुंबई', 'राहुल शर्मा', 'प्रबंधक'),
    ('मुंबई', 'प्रिया पटेल', 'लेखाकार'),
    ('São Paulo', 'João Silva', 'Gerente'),
    ('São Paulo', 'Ana Souza', 'Contadora'),
    ('Zürich', 'Lukas Müller', 'Filialleiter'),
    ('Zürich', 'Anna Brühlmann', 'Buchhalterin')
) as s(city, name, role)
order by city, name
