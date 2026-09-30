namespace Amaterasu {
  export type LocaleMessages = Record<string, unknown>;

  const BASE_MESSAGE_KEYS = [
    "Overview",
    "Library",
    "Settings",
    "Sessions",
    "Applications",
    "Steam",
    "Diagnostics",
    "Host workspace",
    "Sign out",
    "Locale",
    "Loading locale preference…",
    "Saving locale preference…",
    "Read only",
    "This locale translates navigation and selected labels; untranslated text uses English.",
  ] as const;

  const UI_MESSAGE_KEYS = [
    "← Previous",
    "Next →",
    "Refresh",
    "Save",
    "Cancel",
    "Close",
    "Search",
    "Edit",
    "Delete",
    "Try again",
    "Unable to complete request",
  ] as const;

  const SHELL_MESSAGE_KEYS = [
    "WORKSPACE",
    "Development preview",
    "Local management console",
    "Streaming features are in progress.",
    "Toggle navigation",
  ] as const;

  interface LocaleTranslationLayers {
    base: readonly string[];
    ui: readonly string[];
  }

  const localeTranslations: Partial<
    Record<LocaleToken, LocaleTranslationLayers>
  > = {
    bg: {
      base: [
        "Общ преглед", "Библиотека", "Настройки", "Сесии", "Приложения", "Steam", "Диагностика",
        "Работно пространство на хоста", "Изход", "Език", "Зареждане на езиковата настройка…",
        "Запазване на езиковата настройка…", "Само за четене",
        "Тази локализация превежда навигацията и избрани етикети; останалият текст е на английски.",
      ],
      ui: [
        "← Предишна", "Следваща →", "Обновяване", "Запазване", "Отказ", "Затвори", "Търсене",
        "Редактиране", "Изтриване", "Опитай отново", "Заявката не можа да бъде изпълнена",
      ],
    },
    cs: {
      base: [
        "Přehled", "Knihovna", "Nastavení", "Relace", "Aplikace", "Steam", "Diagnostika",
        "Pracovní prostor hostitele", "Odhlásit", "Jazyk", "Načítání jazykového nastavení…",
        "Ukládání jazykového nastavení…", "Pouze pro čtení",
        "Toto locale překládá navigaci a vybrané popisky; ostatní text zůstává v angličtině.",
      ],
      ui: [
        "← Předchozí", "Další →", "Obnovit", "Uložit", "Zrušit", "Zavřít", "Hledat", "Upravit",
        "Odstranit", "Zkusit znovu", "Požadavek se nepodařilo dokončit",
      ],
    },
    de: {
      base: [
        "Übersicht", "Bibliothek", "Einstellungen", "Sitzungen", "Anwendungen", "Steam", "Diagnose",
        "Arbeitsbereich des Hosts", "Abmelden", "Sprache", "Spracheinstellung wird geladen…",
        "Spracheinstellung wird gespeichert…", "Schreibgeschützt",
        "Diese Spracheinstellung übersetzt die Navigation und ausgewählte Beschriftungen; anderer Text bleibt Englisch.",
      ],
      ui: [
        "← Zurück", "Weiter →", "Aktualisieren", "Speichern", "Abbrechen", "Schließen", "Suchen",
        "Bearbeiten", "Löschen", "Erneut versuchen", "Anfrage konnte nicht ausgeführt werden",
      ],
    },
    es: {
      base: [
        "Resumen", "Biblioteca", "Ajustes", "Sesiones", "Aplicaciones", "Steam", "Diagnóstico",
        "Espacio de trabajo del host", "Cerrar sesión", "Idioma", "Cargando la preferencia de idioma…",
        "Guardando la preferencia de idioma…", "Solo lectura",
        "Este idioma traduce la navegación y algunas etiquetas; el resto del texto aparece en inglés.",
      ],
      ui: [
        "← Anterior", "Siguiente →", "Actualizar", "Guardar", "Cancelar", "Cerrar", "Buscar", "Editar",
        "Eliminar", "Intentar de nuevo", "No se pudo completar la solicitud",
      ],
    },
    fr: {
      base: [
        "Vue d’ensemble", "Bibliothèque", "Paramètres", "Sessions", "Applications", "Steam", "Diagnostics",
        "Espace de travail de l’hôte", "Déconnexion", "Langue", "Chargement de la préférence de langue…",
        "Enregistrement de la préférence de langue…", "Lecture seule",
        "Cette langue traduit la navigation et certains libellés ; le reste du texte reste en anglais.",
      ],
      ui: [
        "← Précédent", "Suivant →", "Actualiser", "Enregistrer", "Annuler", "Fermer", "Rechercher",
        "Modifier", "Supprimer", "Réessayer", "Impossible de terminer la requête",
      ],
    },
    hu: {
      base: [
        "Áttekintés", "Könyvtár", "Beállítások", "Munkamenetek", "Alkalmazások", "Steam", "Diagnosztika",
        "A gazdagép munkaterülete", "Kijelentkezés", "Nyelv", "Nyelvi beállítás betöltése…",
        "Nyelvi beállítás mentése…", "Csak olvasható",
        "Ez a nyelv a navigációt és néhány címkét fordít le; a többi szöveg angol marad.",
      ],
      ui: [
        "← Előző", "Következő →", "Frissítés", "Mentés", "Mégse", "Bezárás", "Keresés", "Szerkesztés",
        "Törlés", "Próbálja újra", "A kérést nem sikerült végrehajtani",
      ],
    },
    it: {
      base: [
        "Panoramica", "Libreria", "Impostazioni", "Sessioni", "Applicazioni", "Steam", "Diagnostica",
        "Area di lavoro dell’host", "Esci", "Lingua", "Caricamento della preferenza della lingua…",
        "Salvataggio della preferenza della lingua…", "Sola lettura",
        "Questa lingua traduce la navigazione e alcune etichette; il resto del testo rimane in inglese.",
      ],
      ui: [
        "← Precedente", "Successivo →", "Aggiorna", "Salva", "Annulla", "Chiudi", "Cerca", "Modifica",
        "Elimina", "Riprova", "Impossibile completare la richiesta",
      ],
    },
    ja: {
      base: [
        "概要", "ライブラリ", "設定", "セッション", "アプリケーション", "Steam", "診断",
        "ホストのワークスペース", "サインアウト", "言語", "言語設定を読み込み中…", "言語設定を保存中…",
        "読み取り専用", "この言語ではナビゲーションと一部のラベルが翻訳されます。その他のテキストは英語で表示されます。",
      ],
      ui: [
        "← 前へ", "次へ →", "更新", "保存", "キャンセル", "閉じる", "検索", "編集", "削除", "再試行",
        "リクエストを完了できませんでした",
      ],
    },
    ko: {
      base: [
        "개요", "라이브러리", "설정", "세션", "애플리케이션", "Steam", "진단",
        "호스트 작업 공간", "로그아웃", "언어", "언어 설정을 불러오는 중…", "언어 설정을 저장하는 중…",
        "읽기 전용", "이 언어는 탐색 메뉴와 일부 레이블을 번역합니다. 나머지 텍스트는 영어로 표시됩니다.",
      ],
      ui: [
        "← 이전", "다음 →", "새로 고침", "저장", "취소", "닫기", "검색", "편집", "삭제", "다시 시도",
        "요청을 완료할 수 없습니다",
      ],
    },
    pl: {
      base: [
        "Przegląd", "Biblioteka", "Ustawienia", "Sesje", "Aplikacje", "Steam", "Diagnostyka",
        "Obszar roboczy hosta", "Wyloguj", "Język", "Wczytywanie preferencji języka…",
        "Zapisywanie preferencji języka…", "Tylko do odczytu",
        "Ten język tłumaczy nawigację i wybrane etykiety; pozostały tekst jest wyświetlany po angielsku.",
      ],
      ui: [
        "← Poprzednia", "Następna →", "Odśwież", "Zapisz", "Anuluj", "Zamknij", "Szukaj", "Edytuj", "Usuń",
        "Spróbuj ponownie", "Nie można wykonać żądania",
      ],
    },
    pt: {
      base: [
        "Visão geral", "Biblioteca", "Configurações", "Sessões", "Aplicativos", "Steam", "Diagnóstico",
        "Área de trabalho do host", "Sair", "Idioma", "Carregando a preferência de idioma…",
        "Salvando a preferência de idioma…", "Somente leitura",
        "Este idioma traduz a navegação e alguns rótulos; o restante do texto aparece em inglês.",
      ],
      ui: [
        "← Anterior", "Próxima →", "Atualizar", "Salvar", "Cancelar", "Fechar", "Pesquisar", "Editar",
        "Excluir", "Tentar novamente", "Não foi possível concluir a solicitação",
      ],
    },
    pt_BR: {
      base: [
        "Visão geral", "Biblioteca", "Configurações", "Sessões", "Aplicativos", "Steam", "Diagnóstico",
        "Área de trabalho do host", "Sair", "Idioma", "Carregando a preferência de idioma…",
        "Salvando a preferência de idioma…", "Somente leitura",
        "Este idioma traduz a navegação e alguns rótulos; o restante do texto aparece em inglês.",
      ],
      ui: [
        "← Anterior", "Próxima →", "Atualizar", "Salvar", "Cancelar", "Fechar", "Pesquisar", "Editar",
        "Excluir", "Tentar novamente", "Não foi possível concluir a solicitação",
      ],
    },
    ru: {
      base: [
        "Обзор", "Библиотека", "Настройки", "Сеансы", "Приложения", "Steam", "Диагностика",
        "Рабочая область хоста", "Выйти", "Язык", "Загрузка языковых настроек…", "Сохранение языковых настроек…",
        "Только для чтения", "Этот язык переводит навигацию и отдельные подписи; остальной текст отображается на английском.",
      ],
      ui: [
        "← Назад", "Далее →", "Обновить", "Сохранить", "Отмена", "Закрыть", "Поиск", "Изменить", "Удалить",
        "Повторить попытку", "Не удалось выполнить запрос",
      ],
    },
    sv: {
      base: [
        "Översikt", "Bibliotek", "Inställningar", "Sessioner", "Program", "Steam", "Diagnostik",
        "Värdens arbetsyta", "Logga ut", "Språk", "Läser in språkval…", "Sparar språkval…", "Skrivskyddad",
        "Det här språket översätter navigeringen och vissa etiketter; övrig text visas på engelska.",
      ],
      ui: [
        "← Föregående", "Nästa →", "Uppdatera", "Spara", "Avbryt", "Stäng", "Sök", "Redigera", "Ta bort",
        "Försök igen", "Det gick inte att slutföra begäran",
      ],
    },
    tr: {
      base: [
        "Genel Bakış", "Kitaplık", "Ayarlar", "Oturumlar", "Uygulamalar", "Steam", "Tanılama",
        "Ana makine çalışma alanı", "Oturumu kapat", "Dil", "Dil tercihi yükleniyor…", "Dil tercihi kaydediliyor…",
        "Salt okunur", "Bu dil gezinme menüsünü ve seçili etiketleri çevirir; diğer metinler İngilizce gösterilir.",
      ],
      ui: [
        "← Önceki", "Sonraki →", "Yenile", "Kaydet", "İptal", "Kapat", "Ara", "Düzenle", "Sil", "Tekrar dene",
        "İstek tamamlanamadı",
      ],
    },
    uk: {
      base: [
        "Огляд", "Бібліотека", "Налаштування", "Сеанси", "Програми", "Steam", "Діагностика",
        "Робочий простір хоста", "Вийти", "Мова", "Завантаження мовних налаштувань…",
        "Збереження мовних налаштувань…", "Лише для читання",
        "Ця мова перекладає навігацію та окремі підписи; решта тексту відображається англійською.",
      ],
      ui: [
        "← Назад", "Далі →", "Оновити", "Зберегти", "Скасувати", "Закрити", "Пошук", "Редагувати", "Видалити",
        "Спробувати знову", "Не вдалося виконати запит",
      ],
    },
    vi: {
      base: [
        "Tổng quan", "Thư viện", "Cài đặt", "Phiên", "Ứng dụng", "Steam", "Chẩn đoán",
        "Không gian làm việc của máy chủ", "Đăng xuất", "Ngôn ngữ", "Đang tải tùy chọn ngôn ngữ…",
        "Đang lưu tùy chọn ngôn ngữ…", "Chỉ đọc",
        "Ngôn ngữ này dịch phần điều hướng và một số nhãn; văn bản còn lại sẽ hiển thị bằng tiếng Anh.",
      ],
      ui: [
        "← Trước", "Tiếp →", "Làm mới", "Lưu", "Hủy", "Đóng", "Tìm kiếm", "Chỉnh sửa", "Xóa", "Thử lại",
        "Không thể hoàn tất yêu cầu",
      ],
    },
    zh: {
      base: [
        "概览", "应用库", "设置", "会话", "应用", "Steam", "诊断", "主机工作区", "退出登录", "语言",
        "正在加载语言偏好…", "正在保存语言偏好…", "只读",
        "此语言会翻译导航菜单和部分标签；其他文本将以英语显示。",
      ],
      ui: [
        "← 上一页", "下一页 →", "刷新", "保存", "取消", "关闭", "搜索", "编辑", "删除", "重试", "无法完成请求",
      ],
    },
    zh_TW: {
      base: [
        "概覽", "應用程式庫", "設定", "工作階段", "應用程式", "Steam", "診斷", "主機工作區", "登出", "語言",
        "正在載入語言偏好設定…", "正在儲存語言偏好設定…", "唯讀",
        "此語言會翻譯導覽選單和部分標籤；其他文字會以英文顯示。",
      ],
      ui: [
        "← 上一頁", "下一頁 →", "重新整理", "儲存", "取消", "關閉", "搜尋", "編輯", "刪除", "重試", "無法完成要求",
      ],
    },
  };

  const shellTranslations: Partial<Record<LocaleToken, readonly string[]>> = {
    bg: [
      "РАБОТНО ПРОСТРАНСТВО", "Прегледна версия", "Локална конзола за управление",
      "Функциите за стрийминг са в процес на разработка.", "Превключване на навигацията",
    ],
    cs: [
      "PRACOVNÍ PROSTOR", "Vývojová verze", "Místní konzole pro správu",
      "Funkce streamování jsou ve vývoji.", "Přepnout navigaci",
    ],
    de: [
      "ARBEITSBEREICH", "Entwicklungsvorschau", "Lokale Verwaltungskonsole",
      "Streamingfunktionen sind in Arbeit.", "Navigation umschalten",
    ],
    es: [
      "ESPACIO DE TRABAJO", "Versión de vista previa", "Consola de administración local",
      "Las funciones de streaming están en desarrollo.", "Alternar navegación",
    ],
    fr: [
      "ESPACE DE TRAVAIL", "Aperçu de développement", "Console de gestion locale",
      "Les fonctions de streaming sont en cours de développement.", "Afficher ou masquer la navigation",
    ],
    hu: [
      "MUNKATERÜLET", "Fejlesztői előzetes", "Helyi felügyeleti konzol",
      "A streamelési funkciók fejlesztés alatt állnak.", "Navigáció váltása",
    ],
    it: [
      "AREA DI LAVORO", "Anteprima di sviluppo", "Console di gestione locale",
      "Le funzioni di streaming sono in fase di sviluppo.", "Mostra o nascondi la navigazione",
    ],
    ja: [
      "ワークスペース", "開発プレビュー", "ローカル管理コンソール",
      "ストリーミング機能は開発中です。", "ナビゲーションの表示を切り替え",
    ],
    ko: [
      "작업 공간", "개발 미리 보기", "로컬 관리 콘솔",
      "스트리밍 기능은 개발 중입니다.", "탐색 메뉴 전환",
    ],
    pl: [
      "OBSZAR ROBOCZY", "Wersja podglądowa", "Lokalna konsola zarządzania",
      "Funkcje strumieniowania są w trakcie prac.", "Przełącz nawigację",
    ],
    pt: [
      "ÁREA DE TRABALHO", "Prévia de desenvolvimento", "Console de gerenciamento local",
      "Os recursos de streaming estão em desenvolvimento.", "Alternar navegação",
    ],
    pt_BR: [
      "ÁREA DE TRABALHO", "Prévia de desenvolvimento", "Console de gerenciamento local",
      "Os recursos de streaming estão em desenvolvimento.", "Alternar navegação",
    ],
    ru: [
      "РАБОЧАЯ ОБЛАСТЬ", "Предварительная версия", "Локальная консоль управления",
      "Функции потоковой передачи находятся в разработке.", "Переключить навигацию",
    ],
    sv: [
      "ARBETSYTA", "Förhandsversion", "Lokal hanteringskonsol",
      "Strömningsfunktioner är under utveckling.", "Visa eller dölj navigeringen",
    ],
    tr: [
      "ÇALIŞMA ALANI", "Geliştirme önizlemesi", "Yerel yönetim konsolu",
      "Akış özellikleri geliştirme aşamasındadır.", "Gezinme menüsünü aç veya kapat",
    ],
    uk: [
      "РОБОЧИЙ ПРОСТІР", "Попередня версія", "Локальна консоль керування",
      "Функції потокового передавання ще розробляються.", "Перемкнути навігацію",
    ],
    vi: [
      "KHÔNG GIAN LÀM VIỆC", "Bản xem trước dành cho nhà phát triển", "Bảng điều khiển quản lý cục bộ",
      "Các tính năng phát trực tuyến đang được phát triển.", "Bật hoặc tắt điều hướng",
    ],
    zh: [
      "工作区", "开发预览版", "本地主机管理控制台", "串流功能仍在开发中。", "切换导航菜单",
    ],
    zh_TW: [
      "工作區", "開發預覽版", "本地主機管理主控台", "串流功能仍在開發中。", "切換導覽選單",
    ],
  };

  function catalogFromTranslations(
    keys: readonly string[],
    translations: readonly string[] | undefined,
  ): LocaleMessages {
    const catalog: LocaleMessages = {};
    if (!translations) return catalog;
    if (translations.length !== keys.length) {
      throw new Error("The locale catalog contains an incomplete message list.");
    }
    keys.forEach((key, index) => {
      const translation = translations[index];
      if (translation !== undefined) catalog[key] = translation;
    });
    return catalog;
  }

  function englishCatalog(keys: readonly string[]): LocaleMessages {
    return Object.fromEntries(keys.map((key) => [key, key]));
  }

  export function mergeLocaleMessages(
    base: LocaleMessages,
    overlay: LocaleMessages,
  ): LocaleMessages {
    const result: LocaleMessages = { ...base };
    for (const [key, value] of Object.entries(overlay)) {
      const existing = result[key];
      result[key] =
        value &&
        typeof value === "object" &&
        !Array.isArray(value) &&
        existing &&
        typeof existing === "object" &&
        !Array.isArray(existing)
          ? mergeLocaleMessages(
              existing as LocaleMessages,
              value as LocaleMessages,
            )
          : value;
    }
    return result;
  }

  export function localeMessagesFor(locale: LocaleToken): LocaleMessages {
    const englishBase = englishCatalog(BASE_MESSAGE_KEYS);
    const englishUi = englishCatalog(UI_MESSAGE_KEYS);
    const englishShell = englishCatalog(SHELL_MESSAGE_KEYS);
    const translations = localeTranslations[locale];
    const localeBase = catalogFromTranslations(
      BASE_MESSAGE_KEYS,
      translations?.base,
    );
    const localeUi = catalogFromTranslations(
      UI_MESSAGE_KEYS,
      translations?.ui,
    );
    const localeShell = catalogFromTranslations(
      SHELL_MESSAGE_KEYS,
      shellTranslations[locale],
    );
    return mergeLocaleMessages(
      mergeLocaleMessages(
        mergeLocaleMessages(
          mergeLocaleMessages(englishBase, englishUi),
          englishShell,
        ),
        localeBase,
      ),
      mergeLocaleMessages(localeUi, localeShell),
    );
  }

  export function translateLocaleMessage(
    source: string,
    messages: LocaleMessages,
  ): string {
    const message = messages[source];
    return typeof message === "string" ? message : source;
  }
}
