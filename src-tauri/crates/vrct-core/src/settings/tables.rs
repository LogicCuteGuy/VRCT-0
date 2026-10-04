//! The language, engine and weight-type tables the settings are checked against.
//!
//! Preserved from the model catalog contract at commit `16cb286c`;
//! `tests/settings_system.rs` compares them with the frozen golden fixture.

/// Local translation models, smallest first.
pub const CTRANSLATE2_WEIGHT_TYPES: &[&str] = &[
    "m2m100_418M-ct2-int8",
    "m2m100_1.2B-ct2-int8",
    "nllb-200-distilled-600M-ct2-int8",
    "nllb-200-distilled-1.3B-ct2-int8",
    "nllb-200-3.3B-ct2-int8",
];

/// Local Whisper models.
pub const WHISPER_WEIGHT_TYPES: &[&str] = &[
    "tiny",
    "base",
    "small",
    "medium",
    "large-v1",
    "large-v2",
    "large-v3",
    "large-v3-turbo-int8",
    "large-v3-turbo",
];

/// Translation engines in the order the UI lists them.
pub const TRANSLATION_ENGINES: &[&str] = &[
    "DeepL_API",
    "Google",
    "Bing",
    "Papago",
    "CTranslate2",
    "Plamo_API",
    "Gemini_API",
    "OpenAI_API",
    "LMStudio",
    "OpenAI_Compatible",
    "Ollama",
    "Groq_API",
    "OpenRouter_API",
];

/// Speech-to-text engines.
pub const TRANSCRIPTION_ENGINES: &[&str] = &[
    "Google",
    "Whisper",
    "Groq_Whisper",
    "OpenAI_Whisper",
    "Custom_Whisper",
    "Deepgram",
];

/// Languages the OCR can be told to read.
pub const OCR_SOURCE_LANGUAGES: &[&str] = &[
    "auto",
    "Arabic",
    "Hindi",
    "Korean",
    "Russian",
    "Thai",
    "Ukrainian",
];

/// Transcription language -> the countries it is offered for, in the order Python lists them.
pub const TRANSCRIPTION_LANGUAGES: &[(&str, &[&str])] = &[
    ("Afrikaans", &["South Africa"]),
    ("Albanian", &["Albania"]),
    ("Amharic", &["Ethiopia"]),
    ("Arabic", &["Algeria", "Bahrain", "Egypt", "Israel", "Iraq", "Jordan", "Kuwait", "Lebanon", "Mauritania", "Morocco", "Oman", "Qatar", "Saudi Arabia", "Palestine", "Syria", "Tunisia", "United Arab Emirates", "Yemen"]),
    ("Armenian", &["Armenia"]),
    ("Azerbaijani", &["Azerbaijan"]),
    ("Basque", &["Spain"]),
    ("Bengali", &["Bangladesh", "India"]),
    ("Bosnian", &["Bosnia and Herzegovina"]),
    ("Bulgarian", &["Bulgaria"]),
    ("Burmese", &["Myanmar"]),
    ("Catalan", &["Spain"]),
    ("Chinese Simplified", &["China", "Hong Kong"]),
    ("Chinese Traditional", &["Taiwan", "Hong Kong"]),
    ("Croatian", &["Croatia"]),
    ("Czech", &["Czech Republic"]),
    ("Danish", &["Denmark"]),
    ("Dutch", &["Belgium", "Netherlands"]),
    ("English", &["Australia", "Canada", "Ghana", "Hong Kong", "India", "Ireland", "Kenya", "New Zealand", "Nigeria", "Philippines", "Singapore", "South Africa", "Tanzania", "United Kingdom", "United States"]),
    ("Estonian", &["Estonia"]),
    ("Filipino", &["Philippines"]),
    ("Finnish", &["Finland"]),
    ("French", &["Belgium", "Canada", "France", "Switzerland"]),
    ("Galician", &["Spain"]),
    ("Georgian", &["Georgia"]),
    ("German", &["Austria", "Germany", "Switzerland"]),
    ("Greek", &["Greece"]),
    ("Gujarati", &["India"]),
    ("Hebrew", &["Israel"]),
    ("Hindi", &["India"]),
    ("Hungarian", &["Hungary"]),
    ("Icelandic", &["Iceland"]),
    ("Indonesian", &["Indonesia"]),
    ("Italian", &["Italy", "Switzerland"]),
    ("Japanese", &["Japan"]),
    ("Kannada", &["India"]),
    ("Kazakh", &["Kazakhstan"]),
    ("Khmer", &["Cambodia"]),
    ("Korean", &["South Korea"]),
    ("Lao", &["Laos"]),
    ("Latvian", &["Latvia"]),
    ("Lithuanian", &["Lithuania"]),
    ("Macedonian", &["North Macedonia"]),
    ("Malay", &["Malaysia"]),
    ("Malayalam", &["India"]),
    ("Mongolian", &["Mongolia"]),
    ("Nepali", &["Nepal"]),
    ("Norwegian", &["Norway"]),
    ("Persian", &["Iran"]),
    ("Polish", &["Poland"]),
    ("Portuguese", &["Brazil", "Portugal"]),
    ("Romanian", &["Romania"]),
    ("Russian", &["Russia"]),
    ("Serbian", &["Serbia"]),
    ("Sinhala", &["Sri Lanka"]),
    ("Slovak", &["Slovakia"]),
    ("Slovenian", &["Slovenia"]),
    ("Spanish", &["Argentina", "Bolivia", "Chile", "Colombia", "Costa Rica", "Dominican Republic", "Ecuador", "El Salvador", "Guatemala", "Honduras", "Mexico", "Nicaragua", "Panama", "Paraguay", "Peru", "Puerto Rico", "Spain", "United States", "Uruguay", "Venezuela"]),
    ("Sundanese", &["Indonesia"]),
    ("Swahili", &["Kenya", "Tanzania"]),
    ("Swedish", &["Sweden"]),
    ("Tamil", &["India", "malaysia", "Singapore", "Sri Lanka"]),
    ("Telugu", &["India"]),
    ("Thai", &["Thailand"]),
    ("Turkish", &["Turkey"]),
    ("Ukrainian", &["Ukraine"]),
    ("Urdu", &["India", "Pakistan"]),
    ("Uzbek", &["Uzbekistan"]),
    ("Vietnamese", &["Vietnam"]),
];
