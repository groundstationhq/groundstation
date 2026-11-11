-- Expired sign-ins are swept each time one starts.
CREATE INDEX login_flows_expires ON login_flows (expires_at);
