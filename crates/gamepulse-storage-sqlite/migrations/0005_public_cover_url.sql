ALTER TABLE games ADD COLUMN public_cover_url TEXT
    CHECK (public_cover_url IS NULL OR length(trim(public_cover_url)) > 0);
