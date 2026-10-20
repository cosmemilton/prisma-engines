/*! Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md. */
'use strict'

const assert = require('node:assert/strict')
const fs = require('node:fs')
const path = require('node:path')
const test = require('node:test')

assert.ok(Number(process.versions.node.split('.')[0]) >= 24, 'Run these tests with Node.js 24 or newer')

const packagePath = path.resolve(
  process.env.PRISMA_SCHEMA_WASM || path.join(__dirname, '../../target/prisma-schema-wasm'),
)
const wasmPath = path.join(packagePath, 'src/prisma_schema_build_bg.wasm')
assert.ok(fs.existsSync(wasmPath), `Build the real schema Wasm package first: missing ${wasmPath}`)
const wasmBytes = fs.readFileSync(wasmPath)
assert.ok(wasmBytes.length > 8, 'The generated Wasm module must not be empty')
assert.deepEqual(wasmBytes.subarray(0, 4), Buffer.from([0x00, 0x61, 0x73, 0x6d]), 'Expected a real Wasm module')
const prismaSchema = require(packagePath)

const datasource = `datasource db {
  provider = "firebird"
}
`

const schema = `${datasource}
model Record {
  id       Int      @id @db.Integer
  name     String   @db.VarChar(191)
  total    Decimal  @db.Decimal(18, 6)
  counter  BigInt   @db.BigInt
  ratio    Float    @db.DoublePrecision
  active   Boolean  @db.Boolean
  modified DateTime @db.Timestamp
  payload  Bytes    @db.Blob
}
`

function request(prismaSchemaText) {
  return JSON.stringify({ prismaSchema: prismaSchemaText, noColor: true })
}

function modelWithField(field) {
  return `${datasource}\nmodel Example {\n  id Int @id\n  ${field}\n}\n`
}

function expectValidationError(invalidSchema, patterns) {
  assert.throws(
    () => prismaSchema.validate(request(invalidSchema)),
    (error) => {
      // Validation errors are real wasm-bindgen JsErrors carrying Prisma's JSON
      // diagnostics. A Wasm panic, a loader failure, or an unknown provider does
      // not satisfy these assertions.
      const diagnostic = JSON.parse(error.message)
      assert.equal(diagnostic.error_code, 'P1012')
      assert.doesNotMatch(diagnostic.message, /Datasource provider not known/i)
      for (const pattern of patterns) {
        assert.match(diagnostic.message, pattern)
      }
      return true
    },
  )
}

test('the generated module exports the real schema APIs', () => {
  for (const name of ['get_config', 'validate', 'get_dmmf', 'native_types']) {
    assert.equal(typeof prismaSchema[name], 'function', `Missing Wasm export ${name}`)
  }
})

test('get_config retains Firebird provider identity', () => {
  const result = JSON.parse(prismaSchema.get_config(request(schema)))
  assert.deepEqual(result.errors, [])
  assert.equal(result.config.datasources.length, 1)
  assert.equal(result.config.datasources[0].provider, 'firebird')
  assert.equal(result.config.datasources[0].activeProvider, 'firebird')
})

test('validate accepts the Firebird 3 scalar and native-type baseline', () => {
  assert.doesNotThrow(() => prismaSchema.validate(request(schema)))
})

test('validate accepts Int and BigInt identity primary keys', () => {
  for (const scalar of ['Int', 'BigInt']) {
    const identitySchema = `${datasource}\nmodel Auto {\n  id ${scalar} @id @default(autoincrement())\n}\n`
    assert.doesNotThrow(() => prismaSchema.validate(request(identitySchema)))
  }
})

test('get_dmmf exposes createMany with writable optional identities but keeps bulk returning unavailable', () => {
  const identitySchema = `${datasource}\nmodel Auto {\n  id Int @id @default(autoincrement())\n  code String @unique @db.VarChar(120)\n}\n`
  const result = JSON.parse(prismaSchema.get_dmmf(request(identitySchema)))
  const id = result.datamodel.models.find((model) => model.name === 'Auto')?.fields.find((field) => field.name === 'id')
  assert.deepEqual(id?.default, { name: 'autoincrement', args: [] })
  const operations = result.mappings.modelOperations.find((model) => model.model === 'Auto')
  assert.equal(operations?.createMany, 'createManyAuto')
  assert.equal(operations?.createManyAndReturn, undefined)
  const input = result.schema.inputObjectTypes.prisma.find((type) => type.name === 'AutoCreateManyInput')
  assert.ok(input, 'Expected real createMany input metadata')
  assert.equal(input.fields.find((field) => field.name === 'id')?.isRequired, false)
  const mutation = result.schema.outputObjectTypes.prisma.find((type) => type.name === 'Mutation')
  const createMany = mutation?.fields.find((field) => field.name === 'createManyAuto')
  assert.ok(createMany)
  assert.equal(createMany.args.some((argument) => argument.name === 'skipDuplicates'), false)
})

test('validate retains the conservative single-primary-key identity restriction', () => {
  expectValidationError(modelWithField('counter Int @unique @default(autoincrement())'), [
    /autoincrement/, /non-id field/,
  ])
})

test('get_dmmf exposes the real Unidades string key in checked and unchecked update inputs', () => {
  const text = `${datasource}\nmodel Unidade {\n  codigo String @id @map("CODIGO") @db.VarChar(6)\n  descricao String? @map("DESCRICAO") @db.VarChar(80)\n}\n`
  const result = JSON.parse(prismaSchema.get_dmmf(request(text)))
  for (const name of ['UnidadeUpdateInput', 'UnidadeUncheckedUpdateInput', 'UnidadeUpdateManyMutationInput']) {
    const input = result.schema.inputObjectTypes.prisma.find((type) => type.name === name)
    assert.ok(input, name)
    assert.ok(input.fields.some((field) => field.name === 'codigo'), `${name} must expose codigo`)
  }
  assert.equal(result.datamodel.models[0].fields.find((field) => field.name === 'codigo').dbName, 'CODIGO')
})

test('get_dmmf keeps compound identifiers editable without adding bulk returning', () => {
  const text = `${datasource}\nmodel Compound {\n  tenant String @db.VarChar(6)\n  code Int\n  value String?\n  @@id([tenant, code])\n}\n`
  const result = JSON.parse(prismaSchema.get_dmmf(request(text)))
  for (const name of ['CompoundUpdateInput', 'CompoundUncheckedUpdateInput']) {
    const input = result.schema.inputObjectTypes.prisma.find((type) => type.name === name)
    assert.ok(input, name)
    for (const key of ['tenant', 'code']) assert.ok(input.fields.some((field) => field.name === key), `${name}.${key}`)
  }
  const operations = result.mappings.modelOperations.find((model) => model.model === 'Compound')
  assert.equal(operations.updateManyAndReturn, undefined)
})

test('get_dmmf retains upstream checked auto-generated Int protection', () => {
  const text = `${datasource}\nmodel AutoKey {\n  id Int @id @default(autoincrement())\n  value String?\n}\n`
  const result = JSON.parse(prismaSchema.get_dmmf(request(text)))
  const types = result.schema.inputObjectTypes.prisma
  const checked = types.find((type) => type.name === 'AutoKeyUpdateInput')
  const unchecked = types.find((type) => type.name === 'AutoKeyUncheckedUpdateInput')
  assert.ok(checked)
  assert.ok(unchecked)
  assert.equal(checked.fields.some((field) => field.name === 'id'), false)
  assert.equal(unchecked.fields.some((field) => field.name === 'id'), true)
})

test('get_dmmf preserves the declared Firebird model, scalars, and native types', () => {
  const result = JSON.parse(prismaSchema.get_dmmf(request(schema)))
  const record = result.datamodel.models.find((model) => model.name === 'Record')
  assert.ok(record, 'Expected the validated Record model in DMMF')
  const fields = new Map(record.fields.map((field) => [field.name, field]))
  for (const [name, scalar] of Object.entries({
    id: 'Int', name: 'String', total: 'Decimal', counter: 'BigInt',
    ratio: 'Float', active: 'Boolean', modified: 'DateTime', payload: 'Bytes',
  })) {
    assert.equal(fields.get(name)?.type, scalar)
  }
  assert.equal(fields.get('id').isId, true)
  assert.deepEqual(fields.get('total').nativeType, ['Decimal', ['18', '6']])
  assert.deepEqual(fields.get('name').nativeType, ['VarChar', ['191']])
  assert.deepEqual(fields.get('payload').nativeType, ['Blob', []])
})

test('native_types describes Firebird constructors rather than a PostgreSQL alias', () => {
  const nativeTypes = JSON.parse(prismaSchema.native_types(JSON.stringify(schema)))
  const byName = new Map(nativeTypes.map((nativeType) => [nativeType.name, nativeType]))
  const expected = {
    SmallInt: 'Int', Integer: 'Int', BigInt: 'BigInt', Float: 'Float', DoublePrecision: 'Float',
    Decimal: 'Decimal', Numeric: 'Decimal', Boolean: 'Boolean', Char: 'String', VarChar: 'String',
    Text: 'String', Blob: 'Bytes', Date: 'DateTime', Time: 'DateTime', Timestamp: 'DateTime',
  }
  assert.deepEqual([...byName.keys()].sort(), Object.keys(expected).sort())
  for (const [name, scalar] of Object.entries(expected)) {
    assert.deepEqual(byName.get(name).prisma_types, [scalar], name)
  }
  assert.equal(byName.get('VarChar')._number_of_args, 1)
  assert.equal(byName.get('Decimal')._number_of_optional_args, 2)
})

test('validate rejects the unsupported Json scalar', () => {
  expectValidationError(modelWithField('value Json'), [/Json/, /not support/i])
})

test('validate rejects BigInt paired with the Integer native type', () => {
  expectValidationError(modelWithField('value BigInt @db.Integer'), [/BigInt/, /Integer/, /not compatible/i])
})

test('validate rejects precision 19 above the Firebird 3 maximum', () => {
  expectValidationError(modelWithField('value Decimal @db.Decimal(19, 0)'), [/Decimal/, /18/])
})

test('validate accepts the UTF8-safe string limit and rejects VarChar(8192)', () => {
  assert.doesNotThrow(() => prismaSchema.validate(request(modelWithField('value String @db.VarChar(8191)'))))
  expectValidationError(modelWithField('value String @db.VarChar(8192)'), [/VarChar/, /8191/])
})

test('validate accepts 31-byte mapped identifiers and rejects 32-byte identifiers', () => {
  const mappedModel = (databaseName) => `${datasource}\nmodel Example {\n  id Int @id\n  @@map("${databaseName}")\n}\n`
  assert.doesNotThrow(() => prismaSchema.validate(request(mappedModel('a'.repeat(31)))))
  expectValidationError(mappedModel('a'.repeat(32)), [/31/, /long|length|bytes|characters/i])
})

test('validate checks database column names and permits long Prisma names mapped to short identifiers', () => {
  const longModel = 'M'.repeat(32)
  const longField = 'f'.repeat(32)
  const mappedSchema = `${datasource}\nmodel ${longModel} {\n  ${longField} Int @id @map("id")\n  @@map("short_table")\n}\n`
  assert.doesNotThrow(() => prismaSchema.validate(request(mappedSchema)))
  assert.doesNotThrow(() => prismaSchema.validate(request(modelWithField(`${'a'.repeat(31)} String`))))
  expectValidationError(modelWithField(`${longField} String`), [/database column name/i, /31 bytes/])
  expectValidationError(modelWithField(`value String @map("${longField}")`), [/database column name/i, /31 bytes/])
  const unmappedSchema = `${datasource}\nmodel ${longModel} {\n  id Int @id\n}\n`
  expectValidationError(unmappedSchema, [/database table name/i, /31 bytes/])
})

test('validate explicitly rejects Firebird views even when the views preview is enabled', () => {
  const viewSchema = `${datasource}
generator client {
  provider = "prisma-client"
  previewFeatures = ["views"]
}

view UnsupportedView {
  id Int @unique
}
`
  expectValidationError(viewSchema, [
    /Views are not supported by the experimental Firebird connector\./,
    /UnsupportedView/,
    /Validation Error Count: 1$/,
  ])
})

test('validate rejects supplementary Unicode and trailing ASCII spaces in mapped table and column names', () => {
  const invalidNames = [
    ['item\u{1f600}', /must contain only Unicode BMP characters \(at most U\+FFFF\)/],
    ['item ', /must not end with an ASCII space/],
  ]
  for (const [databaseName, reason] of invalidNames) {
    // Keep these names below the byte limit so only the character rule applies.
    assert.ok(Buffer.byteLength(databaseName, 'utf8') <= 31)
    const mappedTable = `${datasource}\nmodel Example {\n  id Int @id\n  @@map("${databaseName}")\n}\n`
    const mappedColumn = modelWithField(`value String @map("${databaseName}")`)
    for (const [invalidSchema, object] of [[mappedTable, 'table'], [mappedColumn, 'column']]) {
      expectValidationError(invalidSchema, [
        new RegExp(`database ${object} name`),
        reason,
        /Firebird 3\.0/,
        /Validation Error Count: 1$/,
      ])
    }
  }
})

test('get_dmmf isolates server currentDate writes to native Firebird DATE fields', () => {
  for (const fields of [
    'day DateTime @db.Date\n  stamp DateTime @db.Timestamp\n  nullableDay DateTime? @db.Date\n  clock DateTime? @db.Time',
    'stamp DateTime @db.Timestamp\n  clock DateTime? @db.Time\n  nullableDay DateTime? @db.Date\n  day DateTime @db.Date',
  ]) {
    const text = `${datasource}\nmodel DateWrite {\n  id Int @id\n  ${fields}\n}\n`
    const result = JSON.parse(prismaSchema.get_dmmf(request(text)))
    const inputs = new Map(result.schema.inputObjectTypes.prisma.map((type) => [type.name, type]))
    const create = inputs.get('DateWriteCreateInput')
    assert.ok(create)
    const field = (name) => create.fields.find((candidate) => candidate.name === name)
    const types = (name) => field(name).inputTypes.map((type) => type.type)
    assert.deepEqual(types('day'), ['DateTime', 'FirebirdCurrentDateInput'])
    assert.deepEqual(types('nullableDay'), ['DateTime', 'FirebirdCurrentDateInput', 'Null'])
    assert.equal(field('day').isRequired, true)
    assert.equal(field('nullableDay').isRequired, false)
    assert.deepEqual(types('stamp'), ['DateTime'])
    assert.deepEqual(types('clock'), ['DateTime', 'Null'])
    for (const name of ['FirebirdCurrentDateInput', 'FirebirdDateFieldUpdateOperationsInput', 'NullableFirebirdDateFieldUpdateOperationsInput']) {
      const flag = inputs.get(name)?.fields.find((candidate) => candidate.name === 'currentDate')
      assert.ok(flag, name)
      assert.equal(flag.isParameterizable, false)
      assert.deepEqual(flag.inputTypes.map((type) => type.type), ['Boolean'])
    }
    for (const name of ['DateTimeFieldUpdateOperationsInput', 'NullableDateTimeFieldUpdateOperationsInput']) {
      assert.equal(inputs.get(name)?.fields.some((candidate) => candidate.name === 'currentDate'), false)
    }
    const many = inputs.get('DateWriteCreateManyInput')
    assert.ok(many.fields.find((candidate) => candidate.name === 'day').inputTypes.some((type) => type.type === 'FirebirdCurrentDateInput'))
  }
})

test('get_dmmf does not expose the Firebird currentDate operation to PostgreSQL DATE', () => {
  const text = 'datasource db {\n  provider = "postgresql"\n}\nmodel DateWrite {\n  id Int @id\n  day DateTime @db.Date\n}\n'
  const result = JSON.parse(prismaSchema.get_dmmf(request(text)))
  assert.equal(result.schema.inputObjectTypes.prisma.some((type) =>
    type.fields.some((field) => field.name === 'currentDate')), false)
})

// Generated Client CRUD is verified by the paired ORM integration suite at
// docs/plans/firebird/tests/generated-client.test.ts, independently of schema APIs.
