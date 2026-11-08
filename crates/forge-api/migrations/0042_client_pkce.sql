-- PKCE between a client and the API (RFC 7636, S256): a client that starts a login with a
-- `code_challenge` must show the matching `code_verifier` to exchange the login code. The
-- launcher needs it, since any local program could catch the code on its loopback address.
-- NULL for the website, which keeps working without.
ALTER TABLE oauth_states ADD COLUMN client_challenge TEXT;
ALTER TABLE login_codes ADD COLUMN client_challenge TEXT;
