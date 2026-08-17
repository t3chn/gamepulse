ALTER TABLE review_input_excerpts ADD COLUMN polarity TEXT
    CHECK (polarity IS NULL OR polarity IN ('positive', 'negative'));
