from hypothesis import settings

# No example database on disk: CI and local runs leave no files behind.
settings.register_profile("fleet", database=None)
settings.load_profile("fleet")
