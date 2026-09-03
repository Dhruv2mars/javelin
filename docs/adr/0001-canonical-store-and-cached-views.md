# ADR 0001: Canonical store and cached views

Status: accepted

Javelin Store is canonical. Managed views are reconstructable caches plus tentative Layer state. Database state selects accepted truth after crashes; filesystem views never override it.

