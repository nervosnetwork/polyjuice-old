#ifndef CRLP_H_
#define CRLP_H_

/* API */

#define CRLP_ERROR_EOF -1
#define CRLP_ERROR_NOTOKEN -2
#define CRLP_ERROR_INVALID_LENGTH -3

typedef enum { CRLP_STRING = 0, CRLP_LIST = 1 } crlp_type_t;

typedef struct {
  crlp_type_t type;
  union {
    /* substring in original data */
    struct {
      int64_t start_char;
      int64_t end_char;
    } string;
    /* child tokens for lists */
    struct {
      int start_token;
      int end_token;
    } list;
  } data;
  /* This is an internal only flag, users should not rely on anything
   * in this field. Due to C struct alignment, this can be sneaked into
   * the struct without affecting struct size.
   */
  uint8_t _flag;
} crlp_token_t;

int crlp_parse_static(const uint8_t *source, int64_t len, crlp_token_t *tokens,
                      int token_size);
int64_t crlp_assemble(const uint8_t *source, int64_t len,
                      const crlp_token_t *tokens, int token_size,
                      int root_index, uint8_t *out, int64_t out_len);

int crlp_token_is_string(const crlp_token_t *t) {
  return t->type == CRLP_STRING;
}

int64_t crlp_token_string_length(const crlp_token_t *t) {
  return t->data.string.end_char - t->data.string.start_char;
}

const uint8_t *crlp_token_string_pointer(const uint8_t *source, int64_t len,
                                         const crlp_token_t *t) {
  if (t->data.string.end_char > len) {
    return NULL;
  }
  return &source[t->data.string.start_char];
}

int crlp_token_is_list(const crlp_token_t *t) { return t->type == CRLP_LIST; }

int crlp_token_list_size(const crlp_token_t *t) {
  return t->data.list.end_token - t->data.list.start_token;
}

crlp_token_t crlp_create_string_token(int64_t start, int64_t end) {
  crlp_token_t t;
  t.type = CRLP_STRING;
  t._flag = 0;
  t.data.string.start_char = start;
  t.data.string.end_char = end;
  return t;
}

crlp_token_t crlp_create_list_token(int start_token, int end_token) {
  crlp_token_t t;
  t.type = CRLP_LIST;
  t._flag = 0;
  t.data.list.start_token = start_token;
  t.data.list.end_token = end_token;
  return t;
}

/* Implementation */

#define CRLP_I_FLAG_UNPROCESSED_LIST 0x1

typedef struct {
  const uint8_t *source;
  int64_t len;
  crlp_token_t *tokens;
  int token_size;

  int next_token;
} crlp_i_state_t;

int crlp_i_alloc_token(crlp_i_state_t *state) {
  if (state->next_token >= state->token_size) {
    return CRLP_ERROR_NOTOKEN;
  }
  return state->next_token++;
}

int64_t crlp_i_parse_variable_length(const crlp_i_state_t *state, int64_t index,
                                     int len_len) {
  if (index + len_len > state->len) {
    return CRLP_ERROR_EOF;
  }
  int64_t l = 0;
  for (int i = 0; i < len_len; i++) {
    l = (l << 8) | state->source[index + i];
  }
  return l;
}

int crlp_i_parse_single_level_item(const crlp_i_state_t *state, int64_t index,
                                   crlp_type_t *type, int64_t *start_char,
                                   int64_t *end_char) {
  if (index >= state->len) {
    return CRLP_ERROR_EOF;
  }
  uint8_t byte = state->source[index++];
  if (byte < 0x80) {
    *type = CRLP_STRING;
    *start_char = index - 1;
    *end_char = index;
  } else if (byte < 0xB8) {
    int64_t len = byte - 0x80;
    if (index + len > state->len) {
      return CRLP_ERROR_EOF;
    }
    *type = CRLP_STRING;
    *start_char = index;
    *end_char = index + len;
  } else if (byte < 0xC0) {
    int len_len = byte - 0xB7;
    int64_t len = crlp_i_parse_variable_length(state, index, len_len);
    if (len < 0) {
      return len;
    }
    index += len_len;
    if (index + len > state->len) {
      return CRLP_ERROR_EOF;
    }
    *type = CRLP_STRING;
    *start_char = index;
    *end_char = index + len;
  } else if (byte < 0xF8) {
    int64_t len = byte - 0xC0;
    if (index + len > state->len) {
      return CRLP_ERROR_EOF;
    }
    *type = CRLP_LIST;
    *start_char = index;
    *end_char = index + len;
  } else {
    int len_len = byte - 0xF7;
    int64_t len = crlp_i_parse_variable_length(state, index, len_len);
    if (len < 0) {
      return len;
    }
    index += len_len;
    if (index + len > state->len) {
      return CRLP_ERROR_EOF;
    }
    *type = CRLP_LIST;
    *start_char = index;
    *end_char = index + len;
  }
  return 0;
}

int crlp_i_parse_single_level(crlp_i_state_t *state, int64_t start,
                              int64_t end) {
  while (start < end) {
    crlp_type_t type = CRLP_STRING;
    int64_t start_char = 0;
    int64_t end_char = 0;
    int ret = crlp_i_parse_single_level_item(state, start, &type, &start_char,
                                             &end_char);
    if (ret < 0) {
      return ret;
    }
    int i = crlp_i_alloc_token(state);
    if (i < 0) {
      return i;
    }
    switch (type) {
      case CRLP_STRING:
        state->tokens[i] = crlp_create_string_token(start_char, end_char);
        break;
      case CRLP_LIST:
        /*
         * This is not a valid token yet, we are only using it to hold
         * necessary values.
         */
        state->tokens[i].type = CRLP_LIST;
        state->tokens[i].data.string.start_char = start_char;
        state->tokens[i].data.string.end_char = end_char;
        state->tokens[i]._flag = CRLP_I_FLAG_UNPROCESSED_LIST;
        break;
    }
    start = end_char;
  }
  return state->next_token;
}

/*
 * TODO: provide a different realloc version that allows growing
 * tokens as needed.
 */
int crlp_parse_static(const uint8_t *source, int64_t len, crlp_token_t *tokens,
                      int token_size) {
  crlp_i_state_t state;
  state.source = source;
  state.len = len;
  state.tokens = tokens;
  state.token_size = token_size;
  state.next_token = 0;

  int ret = crlp_i_parse_single_level(&state, 0, len);
  if (ret < 0) {
    return ret;
  }

  int found_unprocessed = 1;
  while (found_unprocessed) {
    found_unprocessed = 0;
    for (int i = 0; i < state.next_token; i++) {
      crlp_token_t *token = &state.tokens[i];
      if ((token->_flag & CRLP_I_FLAG_UNPROCESSED_LIST) != 0) {
        found_unprocessed = 1;
        int start_token = state.next_token;
        ret = crlp_i_parse_single_level(&state, token->data.string.start_char,
                                        token->data.string.end_char);
        if (ret < 0) {
          return ret;
        }
        int end_token = state.next_token;
        *token = crlp_create_list_token(start_token, end_token);
      }
    }
  }

  return state.next_token;
}

#define WRITE_CHAR(i, c)     \
  do {                       \
    if ((i) >= out_len) {    \
      return CRLP_ERROR_EOF; \
    }                        \
    if (out) {               \
      out[(i)] = (c);        \
    }                        \
  } while (0)

int64_t crlp_i_encode_length(int64_t len, uint8_t *out, int64_t out_len,
                             uint8_t offset) {
  if (len < 0) {
    return CRLP_ERROR_INVALID_LENGTH;
  } else if (len < 56) {
    WRITE_CHAR(0, len + offset);
    return 1;
  } else {
    uint8_t buffer[8];
    int i;
    for (i = 0; i < 8 && len > 0; i++) {
      buffer[i] = len & 0xFF;
      len >>= 8;
    }
    WRITE_CHAR(0, i + offset + 55);
    for (int j = 0; j < i; j++) {
      WRITE_CHAR(j + 1, buffer[i - j - 1]);
    }
    return i + 1;
  }
}

int64_t crlp_assemble(const uint8_t *source, int64_t len,
                      const crlp_token_t *tokens, int token_size,
                      int root_index, uint8_t *out, int64_t out_len) {
  if (root_index >= token_size) {
    return CRLP_ERROR_EOF;
  }
  const crlp_token_t *token = &tokens[root_index];
  switch (token->type) {
    case CRLP_STRING: {
      int64_t index = token->data.string.start_char;
      int64_t string_length =
          token->data.string.end_char - token->data.string.start_char;
      if (index + string_length > len) {
        return CRLP_ERROR_EOF;
      }
      if (string_length == 1) {
        if (source[index] < 0x80) {
          WRITE_CHAR(0, source[index]);
          return 1;
        }
      }
      int64_t len_len = crlp_i_encode_length(string_length, out, out_len, 0x80);
      if (len_len < 0) {
        return len_len;
      }
      for (int64_t i = 0; i < string_length; i++) {
        WRITE_CHAR(len_len + i, source[index + i]);
      }
      return len_len + string_length;
    }
    case CRLP_LIST: {
      int64_t items_length = 0;
      for (int i = token->data.list.start_token; i < token->data.list.end_token;
           i++) {
        int64_t wrote =
            crlp_assemble(source, len, tokens, token_size, i, NULL, 0xFFFFFFFF);
        if (wrote < 0) {
          return wrote;
        }
        items_length += wrote;
      }
      int64_t current_index =
          crlp_i_encode_length(items_length, out, out_len, 0xc0);
      if (current_index < 0) {
        return current_index;
      }
      for (int i = token->data.list.start_token; i < token->data.list.end_token;
           i++) {
        int64_t wrote =
            crlp_assemble(source, len, tokens, token_size, i,
                          out + current_index, out_len - current_index);
        if (wrote < 0) {
          return wrote;
        }
        current_index += wrote;
      }
      return current_index;
    }
  }
  return CRLP_ERROR_EOF;
}

#undef WRITE_CHAR

#endif /* CRLP_H_ */
